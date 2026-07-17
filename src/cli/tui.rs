use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use chrono::{DateTime, Local, Utc};
use std::str::FromStr;

use crate::core::{Agent, AppConfig, DaemonRequest, DaemonResponse};
use super::commands::daemon_request;

const REFRESH: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);

// ── Helpers ───────────────────────────────────────────────────────────────────

fn abbrev(name: &str) -> String {
    const MAX: usize = 10;
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= MAX {
        name.to_string()
    } else {
        chars[..MAX - 1].iter().collect::<String>() + "…"
    }
}

/// Compact token count: 12345 -> "12.3k tok", 2_100_000 -> "2.1M tok".
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M tok", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k tok", n as f64 / 1_000.0)
    } else {
        format!("{} tok", n)
    }
}

/// Compact magnitude of a duration in seconds: "<1m", "45m", "2h", "3d". Used
/// for both "next run in" and "last run ago" — the column header says which.
fn fmt_delta(secs: i64) -> String {
    let s = secs.abs();
    if s < 60 { "<1m".to_string() }
    else if s < 3600 { format!("{}m", s / 60) }
    else if s < 86_400 { format!("{}h", s / 3600) }
    else { format!("{}d", s / 86_400) }
}

/// Next scheduled fire for an agent as a relative string ("45m", "2h"), or "—"
/// for on-demand (mode: once) agents and unparseable schedules. Cron is evaluated
/// in local time to match the scheduler (which fires in the server timezone).
fn fmt_next_run(agent: &serde_json::Value) -> String {
    if agent["execution_mode"].as_str() != Some("scheduled") {
        return "—".to_string();
    }
    let Some(expr) = agent["schedule"].as_str() else { return "—".to_string(); };
    let Ok(schedule) = cron::Schedule::from_str(expr) else { return "—".to_string(); };
    let now = Local::now();
    match schedule.after(&now).next() {
        Some(next) => fmt_delta((next - now).num_seconds()),
        None => "—".to_string(),
    }
}

/// Per-agent run summary for the list. Last-run comes from the agent's own
/// authoritative `last_run` field (always current); success rate is best-effort
/// over the recently-loaded executions ("—" when none are in the window).
/// Returns ("2h"|"—", "100%"|"—").
fn agent_run_summary(agent: &serde_json::Value, execs: &[serde_json::Value]) -> (String, String) {
    let last_str = agent["last_run"].as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| fmt_delta((Utc::now() - t.with_timezone(&Utc)).num_seconds()))
        .unwrap_or_else(|| "—".to_string());

    let agent_id = agent["id"].as_str().unwrap_or("");
    let (mut done, mut failed) = (0u32, 0u32);
    for e in execs {
        if e["agent_id"].as_str() != Some(agent_id) { continue; }
        match e["status"].as_str() {
            Some("completed") => done += 1,
            Some("failed") => failed += 1,
            _ => {}
        }
    }
    let succ_str = if done + failed == 0 {
        "—".to_string()
    } else {
        format!("{}%", done * 100 / (done + failed))
    };
    (last_str, succ_str)
}

fn fmt_ts(ts: &str) -> String {
    if ts.len() < 16 { return ts.to_string(); }
    let date = &ts[..10];
    let time = &ts[11..16];
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() < 3 { return format!("{} {}", date, time); }
    let mon = match parts[1] {
        "01" => "Jan", "02" => "Feb", "03" => "Mar", "04" => "Apr",
        "05" => "May", "06" => "Jun", "07" => "Jul", "08" => "Aug",
        "09" => "Sep", "10" => "Oct", "11" => "Nov", "12" => "Dec",
        _ => parts[1],
    };
    format!("{} {} {}", mon, parts[2], time)
}

fn extract_task_complete(text: &str) -> String {
    // Strip the TASK_COMPLETE: marker but keep the whole message — models vary on
    // whether the answer comes before or after it (see deep_agent.rs).
    if text.contains("TASK_COMPLETE:") {
        return text.replacen("TASK_COMPLETE:", "", 1).trim().to_string();
    }
    text.trim().to_string()
}

fn panel_block<'a>(title: impl Into<String>, active: bool) -> Block<'a> {
    let border_color = if active { Color::Cyan } else { Color::from_u32(0x2a2a2a) };
    let title_style = if active {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let t = title.into();
    Block::default()
        .title(Span::styled(format!(" {} ", t), title_style))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
}

fn agent_color(name: &str) -> Color {
    const PALETTE: &[(u8, u8, u8)] = &[
        (220, 150,  40), // amber
        ( 70, 150, 230), // blue
        (170,  80, 220), // purple
        ( 40, 200, 150), // teal
        (220, 100,  60), // orange
        (210,  80, 150), // pink
        ( 80, 200, 100), // green
        (200, 190,  60), // yellow
    ];
    let hash = name.bytes().fold(0usize, |acc, b| acc.wrapping_add(b as usize));
    let (r, g, b) = PALETTE[hash % PALETTE.len()];
    Color::Rgb(r, g, b)
}

// ── Domain types ──────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone, Copy)]
enum Panel {
    Agents,
    Executions,
}

impl Panel {
    // View-only dashboard: focus just toggles between the agent list and its detail.
    fn next(self) -> Self {
        match self {
            Panel::Agents => Panel::Executions,
            Panel::Executions => Panel::Agents,
        }
    }
    fn prev(self) -> Self {
        self.next()
    }
}

// ── App state ─────────────────────────────────────────────────────────────────

struct App {
    config: AppConfig,
    agents: Vec<serde_json::Value>,
    executions: Vec<serde_json::Value>,
    selected_agent: usize,
    active_panel: Panel,
    exec_scroll: u16,
    daemon_ok: bool,
    daemon_version: String,
    last_refresh: Instant,
}

impl App {
    fn new(config: AppConfig) -> Self {
        Self {
            config,
            agents: vec![],
            executions: vec![],
            selected_agent: 0,
            active_panel: Panel::Agents,
            exec_scroll: 0,
            daemon_ok: false,
            daemon_version: String::new(),
            last_refresh: Instant::now() - Duration::from_secs(60),
        }
    }

    fn active_agent(&self) -> Option<&serde_json::Value> {
        self.agents.get(self.selected_agent)
    }

    fn active_name(&self) -> String {
        self.active_agent()
            .and_then(|a| a["name"].as_str())
            .unwrap_or("—")
            .to_string()
    }

    fn active_short(&self) -> String {
        abbrev(&self.active_name())
    }

    fn runs_for_active(&self) -> Vec<&serde_json::Value> {
        let agent_id = self.active_agent()
            .and_then(|a| a["id"].as_str())
            .unwrap_or("");
        let mut runs: Vec<_> = self.executions.iter()
            .filter(|e| e["agent_id"].as_str() == Some(agent_id))
            .collect();
        runs.reverse();
        runs
    }

    async fn refresh(&mut self) {
        self.last_refresh = Instant::now();

        match daemon_request(&self.config, DaemonRequest::Ping).await {
            Ok(DaemonResponse::Status { running, version, .. }) => {
                self.daemon_ok = running;
                self.daemon_version = version;
            }
            _ => { self.daemon_ok = false; return; }
        }

        if let Ok(DaemonResponse::AgentList { agents }) =
            daemon_request(&self.config, DaemonRequest::ListAgents).await
        {
            self.agents = agents;
            if !self.agents.is_empty() {
                self.selected_agent = self.selected_agent.min(self.agents.len() - 1);
            }
        }

        if let Ok(DaemonResponse::ExecutionList { executions }) =
            daemon_request(&self.config, DaemonRequest::ListExecutions { limit: 100 }).await
        {
            self.executions = executions;
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run_tui(config: AppConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    // Use inline rendering (no alternate screen) for compatibility with all terminals
    // including Warp, which renders the alternate buffer as an invisible block.
    execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)?;

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), crossterm::cursor::Show, LeaveAlternateScreen);
        original_hook(info);
    }));

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let mut app = App::new(config);
    app.refresh().await;

    loop {
        terminal.draw(|f| render(f, &mut app))?;

        if event::poll(POLL)? {
            if let Event::Key(key) = event::read()? {
                if handle_key(&mut app, key.code, key.modifiers).await {
                    break;
                }
            }
        }

        if app.last_refresh.elapsed() >= REFRESH {
            app.refresh().await;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), crossterm::cursor::Show, LeaveAlternateScreen)?;
    Ok(())
}

// ── Key handler ───────────────────────────────────────────────────────────────

/// View-only key handling: navigate, scroll, refresh, quit. The dashboard never
/// mutates anything — creating, editing and deleting live in the MIND chat and the
/// CLI — so no key here can change your system.
async fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    if code == KeyCode::Char('c') && mods == KeyModifiers::CONTROL {
        return true;
    }

    match code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('r') => app.refresh().await,
        KeyCode::Tab | KeyCode::Right => app.active_panel = app.active_panel.next(),
        KeyCode::BackTab | KeyCode::Left => app.active_panel = app.active_panel.prev(),

        // j/k (and arrows) pick an agent, or scroll the detail when it has focus.
        KeyCode::Down | KeyCode::Char('j') => match app.active_panel {
            Panel::Executions => app.exec_scroll = app.exec_scroll.saturating_add(1),
            _ => {
                if !app.agents.is_empty() {
                    app.selected_agent = (app.selected_agent + 1).min(app.agents.len() - 1);
                    app.exec_scroll = 0;
                }
            }
        },
        KeyCode::Up | KeyCode::Char('k') => match app.active_panel {
            Panel::Executions => app.exec_scroll = app.exec_scroll.saturating_sub(1),
            _ => {
                app.selected_agent = app.selected_agent.saturating_sub(1);
                app.exec_scroll = 0;
            }
        },
        _ => {}
    }

    false
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    // View-only monitoring layout: header line, then agents list + detail side by
    // side, then an activity graph across the bottom. No chat/composer, no
    // standalone tools panel — tools/KBs live in the per-agent detail.
    let [topbar, main, graph_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(6),
    ]).areas(area);

    let [agents_area, detail_area] = Layout::horizontal([
        Constraint::Percentage(38),
        Constraint::Percentage(62),
    ]).areas(main);

    render_topbar(f, app, topbar);
    render_agents(f, app, agents_area);
    render_executions(f, app, detail_area);
    render_activity(f, app, graph_area);
}

/// Token-usage timeline — tokens spent across ALL agents over the last 7 days,
/// bucketed to the panel's full width (newest on the right). A bucket that
/// contained a FAILED run is drawn in red so a broken/expensive spike stands out;
/// healthy usage is brand orange. Bars are built from block glyphs so each column
/// can carry its own colour.
fn render_activity(f: &mut Frame, app: &App, area: Rect) {
    const WINDOW_MIN: i64 = 7 * 24 * 60; // 7 days
    let n = (area.width.saturating_sub(2) as usize).max(1);
    let now = Utc::now();

    let mut tokens = vec![0u64; n];
    let mut failed = vec![false; n];
    for e in &app.executions {
        let Some(ts) = e["started_at"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        else {
            continue;
        };
        let mins_ago = (now - ts.with_timezone(&Utc)).num_minutes();
        if !(0..WINDOW_MIN).contains(&mins_ago) {
            continue;
        }
        let frac = mins_ago as f64 / WINDOW_MIN as f64; // 0=now, 1=oldest
        let idx = (((1.0 - frac) * (n as f64 - 1.0)).round() as usize).min(n - 1);
        tokens[idx] += e["metadata"]["total_tokens"].as_u64().unwrap_or(0);
        if e["status"].as_str() == Some("failed") {
            failed[idx] = true;
        }
    }

    let total_tokens: u64 = tokens.iter().sum();
    let any_failed = failed.iter().any(|&f| f);
    let max = *tokens.iter().max().unwrap_or(&0);

    let title = format!(
        " tokens · all agents · {} / 7d{}  (← older · newer →) ",
        fmt_tokens(total_tokens),
        if any_failed { "  ⚠ failures in red" } else { "" },
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::from_u32(0x3A3A3A)))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if max == 0 {
        f.render_widget(
            Paragraph::new(Span::styled("  no token usage in the last 7 days", Style::default().fg(Color::DarkGray))),
            inner,
        );
        return;
    }

    // Build the bar chart top-down: one Line per row, one Span per column, so each
    // column keeps its own colour. Eighth-block glyphs give sub-cell resolution.
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let h = inner.height as usize;
    let orange = Color::from_u32(0xFF7A45);
    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for row in 0..h {
        let from_bottom = h - 1 - row; // 0 = bottom row
        let mut spans: Vec<Span> = Vec::with_capacity(n);
        for x in 0..n {
            let filled = tokens[x] as f64 / max as f64 * h as f64; // height in cells
            let level = filled - from_bottom as f64; // how full this cell is (0..1+)
            let color = if failed[x] { Color::Red } else { orange };
            let ch = if level >= 1.0 {
                '█'
            } else if level <= 0.0 {
                ' '
            } else {
                BLOCKS[((level * 8.0).round() as usize).clamp(1, 8) - 1]
            };
            spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ── Topbar ─────────────────────────────────────────────────────────────────────

fn render_topbar(f: &mut Frame, app: &App, area: Rect) {
    let ver = if app.daemon_version.is_empty() { "—".to_string() } else { format!("v{}", app.daemon_version) };
    let (dot, dcol) = if app.daemon_ok { ("●", Color::Green) } else { ("✗", Color::Red) };

    let spans = vec![
        // Brand mark only — the `a*` glyph, not the word "agenta" (avoids
        // duplicating the name shown elsewhere).
        Span::styled("a* ", Style::default().fg(Color::from_u32(0xFF7A45)).add_modifier(Modifier::BOLD)),
        Span::styled(ver, Style::default().fg(Color::DarkGray)),
        Span::styled("  daemon ", Style::default().fg(Color::from_u32(0x3a3a3a))),
        Span::styled(dot, Style::default().fg(dcol)),
        Span::styled(
            "  Tab:panel  j/k:nav  r:refresh  q:quit",
            Style::default().fg(Color::from_u32(0x3a3a3a)),
        ),
    ];

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── Agents panel (top-left) ────────────────────────────────────────────────────

fn render_agents(f: &mut Frame, app: &App, area: Rect) {
    let active = app.active_panel == Panel::Agents;
    let block = panel_block("agents", active);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // View-only: the list fills the panel — no overlays, no action footer.
    let list_area = inner;

    if app.agents.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("  no agents", Style::default().fg(Color::DarkGray))),
                Line::from(""),
                Line::from(Span::styled("  create one in MIND chat", Style::default().fg(Color::DarkGray))),
            ]),
            list_area,
        );
    } else {
        let dim = Style::default().fg(Color::from_u32(0x3a3a3a));
        // Fixed column header on the first row; agent rows scroll beneath it.
        let [header_area, rows_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
        ]).areas(list_area);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("   {:<7}", "agent"), dim),
                Span::styled(format!("{:>5}", "next"), dim),
                Span::styled(format!(" {:>4}", "last"), dim),
                Span::styled(format!(" {:>4}", "ok"), dim),
            ])),
            header_area,
        );

        let mut lines: Vec<Line> = vec![];
        for (i, agent) in app.agents.iter().enumerate() {
            let name = agent["name"].as_str().unwrap_or("?");
            let short = abbrev(name);
            let status = agent["status"].as_str().unwrap_or("idle");

            let next = fmt_next_run(agent);
            let (last, succ) = agent_run_summary(agent, &app.executions);

            let (dot, dot_color) = match status {
                "running" | "Running" => ("▶", Color::Green),
                "failed"  | "Failed"  => ("✗", Color::Red),
                _                     => ("●", Color::DarkGray),
            };

            let selected = i == app.selected_agent;
            let name_color = if selected { Color::Cyan } else { agent_color(name) };
            let name_style = if selected {
                Style::default().fg(name_color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(name_color)
            };
            let prefix = if selected { "▶ " } else { "  " };

            // next-run stands out only when there is one; "—" stays dim.
            let next_style = if next == "—" { dim } else { Style::default().fg(Color::from_u32(0xFF7A45)) };
            // success rate: green ≥90%, red <60%, dim otherwise (and for "—").
            let succ_style = match succ.trim_end_matches('%').parse::<u32>() {
                Ok(p) if p >= 90 => Style::default().fg(Color::Green),
                Ok(p) if p < 60  => Style::default().fg(Color::Red),
                Ok(_)            => Style::default().fg(Color::Gray),
                Err(_)           => dim,
            };

            lines.push(Line::from(vec![
                Span::raw(prefix),
                Span::styled(dot, Style::default().fg(dot_color)),
                Span::raw(" "),
                Span::styled(format!("{:<7}", short.chars().take(7).collect::<String>()), name_style),
                Span::styled(format!("{:>5}", next), next_style),
                Span::styled(format!(" {:>4}", last), Style::default().fg(Color::from_u32(0x777777))),
                Span::styled(format!(" {:>4}", succ), succ_style),
            ]));
        }

        let h = rows_area.height as usize;
        let scroll = if app.selected_agent >= h {
            (app.selected_agent + 1).saturating_sub(h) as u16
        } else { 0 };

        f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), rows_area);
    }

}

// ── Executions panel (top-right) ──────────────────────────────────────────────

fn render_executions(f: &mut Frame, app: &App, area: Rect) {
    let short = app.active_short();
    let active = app.active_panel == Panel::Executions;
    let block = panel_block(short.clone(), active);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let runs = app.runs_for_active();
    let w = inner.width as usize;
    let mut lines: Vec<Line> = vec![];

    // ── Agent info: status · model · mode/schedule · tools · KBs · stats ──
    let dim = Style::default().fg(Color::DarkGray);
    let val = Style::default().fg(Color::Gray);
    if let Some(agent) = app.active_agent() {
        let status = agent["status"].as_str().unwrap_or("?");
        let scolor = match status {
            "running" | "active" => Color::Green,
            _ => Color::DarkGray,
        };
        let model = agent["model"].as_str().unwrap_or("—");
        let mode = agent["execution_mode"].as_str().unwrap_or("—");
        let mode_line = match agent["schedule"].as_str() {
            Some(s) if mode == "scheduled" => format!("{}  {}", mode, s),
            _ => mode.to_string(),
        };
        lines.push(Line::from(vec![
            Span::styled(" status ", dim), Span::styled(status.to_string(), Style::default().fg(scolor)),
            Span::styled("   model ", dim), Span::styled(model.to_string(), val),
        ]));
        lines.push(Line::from(vec![Span::styled(" mode   ", dim), Span::styled(mode_line, val)]));

        let tools = agent["tools"].as_array().map(|arr| {
            let n: Vec<&str> = arr.iter().filter_map(|t| t["name"].as_str()).collect();
            if n.is_empty() { "—".to_string() } else { n.join(", ") }
        }).unwrap_or_else(|| "—".to_string());
        lines.push(Line::from(vec![Span::styled(" tools  ", dim), Span::styled(tools, val)]));

        let kbs = agent["config"]["knowledge_bases"].as_array().map(|arr| {
            let n: Vec<&str> = arr.iter().filter_map(|k| k.as_str()).collect();
            if n.is_empty() { "—".to_string() } else { n.join(", ") }
        }).unwrap_or_else(|| "—".to_string());
        lines.push(Line::from(vec![Span::styled(" KBs    ", dim), Span::styled(kbs, val)]));

        let total = runs.len();
        let completed = runs.iter().filter(|e| e["status"].as_str() == Some("completed")).count();
        let failed = runs.iter().filter(|e| e["status"].as_str() == Some("failed")).count();
        let success = if total > 0 { format!("{}%", completed * 100 / total) } else { "—".to_string() };
        let durs: Vec<i64> = runs.iter().filter_map(|e| {
            let s = DateTime::parse_from_rfc3339(e["started_at"].as_str()?).ok()?;
            let c = DateTime::parse_from_rfc3339(e["completed_at"].as_str()?).ok()?;
            Some((c - s).num_seconds().max(0))
        }).collect();
        let avg = if durs.is_empty() { "—".to_string() } else { format!("{}s", durs.iter().sum::<i64>() / durs.len() as i64) };
        let tok_sum: u64 = runs.iter().map(|e| e["metadata"]["total_tokens"].as_u64().unwrap_or(0)).sum();
        lines.push(Line::from(vec![
            Span::styled(" runs   ", dim), Span::styled(total.to_string(), val),
            Span::styled("  ✓", dim), Span::styled(success, Style::default().fg(Color::Green)),
            Span::styled("  avg ", dim), Span::styled(avg, val),
            Span::styled("  ✗", dim),
            Span::styled(failed.to_string(), if failed > 0 { Style::default().fg(Color::Red) } else { dim }),
            Span::styled("  🪙 ", dim), Span::styled(fmt_tokens(tok_sum), val),
        ]));

        // Context-fullness meter: peak input tokens vs the agent's context window.
        // Yellow past 70%, red past 90% — an agent brushing the cap is truncating.
        let window = agent["config"]["context_window"].as_u64().unwrap_or(0);
        let peak = runs.iter()
            .map(|e| e["metadata"]["peak_context_tokens"].as_u64().unwrap_or(0))
            .max().unwrap_or(0);
        if window > 0 && peak > 0 {
            let pct = (peak * 100 / window).min(999);
            let ccolor = if pct >= 90 { Color::Red } else if pct >= 70 { Color::Yellow } else { Color::Gray };
            lines.push(Line::from(vec![
                Span::styled(" context", dim),
                Span::styled(format!(" {} / {}", peak, window), Style::default().fg(ccolor)),
                Span::styled(format!(" ({}%) peak", pct), dim),
            ]));
        }
        lines.push(Line::from(Span::styled(" ── recent runs ──────────", Style::default().fg(Color::from_u32(0x3a3a3a)))));
    }

    if runs.is_empty() {
        lines.push(Line::from(Span::styled("  no runs yet", dim)));
    }

    for (i, e) in runs.iter().enumerate() {
        let status = e["status"].as_str().unwrap_or("?");
        let ts = e["started_at"].as_str().map(fmt_ts).unwrap_or_else(|| "—".to_string());
        let input_preview: String = e["input"].as_str().unwrap_or("")
            .chars()
            .take(w.saturating_sub(22))
            .collect();
        let output_preview: String = e["output"].as_str()
            .map(|o| extract_task_complete(o))
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(w.saturating_sub(22))
            .collect();

        let (badge, badge_color) = match status {
            "completed" => ("✓", Color::Green),
            "running"   => ("▶", Color::Yellow),
            "failed"    => ("✗", Color::Red),
            "cancelled" => ("○", Color::DarkGray),
            _           => ("·", Color::DarkGray),
        };

        let num = format!("#{}", runs.len() - i);
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<5}", num), Style::default().fg(Color::DarkGray)),
            Span::styled(badge, Style::default().fg(badge_color)),
            Span::styled(format!("  {:<14}", ts), Style::default().fg(Color::DarkGray)),
            Span::styled(input_preview, Style::default().fg(Color::White)),
        ]));
        if !output_preview.is_empty() {
            lines.push(Line::from(vec![
                Span::raw("       "),
                Span::styled(output_preview, Style::default().fg(Color::from_u32(0x555555))),
            ]));
        }
    }

    let max_scroll = (lines.len() as u16).saturating_sub(inner.height);
    f.render_widget(
        Paragraph::new(lines).scroll((app.exec_scroll.min(max_scroll), 0)),
        inner,
    );
}

// ── Chat panel (bottom-left) ───────────────────────────────────────────────────

// ── Tools panel (bottom-right) ────────────────────────────────────────────────
