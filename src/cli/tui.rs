use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::core::{AppConfig, DaemonRequest, DaemonResponse};
use super::commands::daemon_request;

const REFRESH: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);
const STOP_WORDS: &[&str] = &["and", "for", "of", "the", "a", "an", "to", "in"];

// ── Helpers ───────────────────────────────────────────────────────────────────

fn abbrev(name: &str) -> String {
    let s: String = name
        .split('-')
        .filter(|w| !STOP_WORDS.contains(w))
        .filter_map(|w| w.chars().next())
        .take(6)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if s.is_empty() {
        name.chars().take(4).map(|c| c.to_ascii_uppercase()).collect()
    } else {
        s
    }
}

fn fmt_ts(ts: &str) -> String {
    // "2026-06-19T10:01:23.456Z" → "Jun 19 10:01"
    if ts.len() < 16 {
        return ts.to_string();
    }
    let date = &ts[..10]; // 2026-06-19
    let time = &ts[11..16]; // 10:01
    let parts: Vec<&str> = date.split('-').collect();
    if parts.len() < 3 {
        return format!("{} {}", date, time);
    }
    let mon = match parts[1] {
        "01" => "Jan", "02" => "Feb", "03" => "Mar", "04" => "Apr",
        "05" => "May", "06" => "Jun", "07" => "Jul", "08" => "Aug",
        "09" => "Sep", "10" => "Oct", "11" => "Nov", "12" => "Dec",
        _ => parts[1],
    };
    format!("{} {} {}", mon, parts[2], time)
}

fn extract_task_complete(text: &str) -> String {
    if let Some(pos) = text.rfind("TASK_COMPLETE:") {
        let after = text[pos + "TASK_COMPLETE:".len()..].trim();
        if !after.is_empty() {
            return after.to_string();
        }
    }
    // Return raw output if no TASK_COMPLETE marker
    text.trim().to_string()
}

// ── Domain types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum ExecStatus {
    Running,
    Completed,
    Failed(String),
}

#[derive(Debug, Clone)]
struct ChatMsg {
    run_num: usize,
    input: String,
    response: Option<String>,
    status: ExecStatus,
    started_at: String,
    duration_secs: Option<i64>,
}

struct Pending {
    exec_id: String,
    agent_name: String,
    msg_idx: usize,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum AgentTab {
    Chat,
    Runs,
    Config,
}

#[derive(Debug, PartialEq)]
enum Focus {
    Content,
    Composer,
}

// ── App state ─────────────────────────────────────────────────────────────────

struct App {
    config: AppConfig,
    agents: Vec<serde_json::Value>,
    selected_agent: usize,
    tab_offset: usize,
    agent_tab: AgentTab,
    chat: HashMap<String, Vec<ChatMsg>>,
    pending: Option<Pending>,
    executions: Vec<serde_json::Value>,
    focus: Focus,
    scroll: u16,
    composer_input: String,
    daemon_ok: bool,
    daemon_version: String,
    status_msg: Option<(String, Instant)>,
    last_refresh: Instant,
}

impl App {
    fn new(config: AppConfig) -> Self {
        Self {
            config,
            agents: vec![],
            selected_agent: 0,
            tab_offset: 0,
            agent_tab: AgentTab::Chat,
            chat: HashMap::new(),
            pending: None,
            executions: vec![],
            focus: Focus::Content,
            scroll: 0,
            composer_input: String::new(),
            daemon_ok: false,
            daemon_version: String::new(),
            status_msg: None,
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

    fn active_chat(&self) -> &[ChatMsg] {
        let name = self.active_name();
        self.chat.get(&name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    fn run_count_for(&self, agent_name: &str) -> usize {
        let agent_id = self.agents.iter()
            .find(|a| a["name"].as_str() == Some(agent_name))
            .and_then(|a| a["id"].as_str())
            .unwrap_or("");
        self.executions.iter()
            .filter(|e| e["agent_id"].as_str() == Some(agent_id))
            .count()
    }

    fn runs_for_active(&self) -> Vec<&serde_json::Value> {
        let agent_id = self.active_agent()
            .and_then(|a| a["id"].as_str())
            .unwrap_or("");
        self.executions.iter()
            .filter(|e| e["agent_id"].as_str() == Some(agent_id))
            .collect()
    }

    fn next_run_num(&self) -> usize {
        let name = self.active_name();
        self.chat.get(&name).map(|v| v.len() + 1).unwrap_or(1)
    }

    fn select_agent(&mut self, idx: usize) {
        self.selected_agent = idx;
        self.scroll = 0;
        self.agent_tab = AgentTab::Chat;
        self.focus = Focus::Content;
    }

    async fn refresh(&mut self) {
        self.last_refresh = Instant::now();

        match daemon_request(&self.config, DaemonRequest::Ping).await {
            Ok(DaemonResponse::Status { running, version, .. }) => {
                self.daemon_ok = running;
                self.daemon_version = version;
            }
            _ => {
                self.daemon_ok = false;
                return;
            }
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
            daemon_request(&self.config, DaemonRequest::ListExecutions { limit: 50 }).await
        {
            self.executions = executions;
        }

        self.poll_pending().await;
    }

    async fn poll_pending(&mut self) {
        let pending = match &self.pending {
            Some(p) => (p.exec_id.clone(), p.agent_name.clone(), p.msg_idx),
            None => return,
        };

        if let Ok(DaemonResponse::ExecutionResult { result }) =
            daemon_request(&self.config, DaemonRequest::GetExecution { id: pending.0.clone() }).await
        {
            let status = result["status"].as_str().unwrap_or("running");
            match status {
                "completed" => {
                    let raw_output = result["output"].as_str().unwrap_or("").to_string();
                    let response = extract_task_complete(&raw_output);
                    let started = result["started_at"].as_str().unwrap_or("").to_string();
                    let completed = result["completed_at"].as_str().unwrap_or("");
                    let duration = if !started.is_empty() && !completed.is_empty() {
                        // rough duration from timestamps (seconds)
                        None // compute later if needed
                    } else {
                        None
                    };

                    if let Some(msgs) = self.chat.get_mut(&pending.1) {
                        if let Some(msg) = msgs.get_mut(pending.2) {
                            msg.response = Some(response);
                            msg.status = ExecStatus::Completed;
                            msg.duration_secs = duration;
                        }
                    }
                    self.pending = None;
                }
                "failed" | "cancelled" => {
                    let err = result["error"].as_str().unwrap_or("unknown error").to_string();
                    if let Some(msgs) = self.chat.get_mut(&pending.1) {
                        if let Some(msg) = msgs.get_mut(pending.2) {
                            msg.response = Some(format!("✗ {}", err));
                            msg.status = ExecStatus::Failed(err);
                        }
                    }
                    self.pending = None;
                }
                _ => {} // still running, keep pending
            }
        }
    }

    async fn send_message(&mut self) {
        let input = self.composer_input.trim().to_string();
        if input.is_empty() {
            return;
        }
        if !self.daemon_ok {
            self.set_status("✗ daemon not running");
            return;
        }
        if self.pending.is_some() {
            self.set_status("✗ waiting for previous response...");
            return;
        }

        let agent_name = self.active_name();
        if agent_name == "—" {
            self.set_status("✗ no agent selected");
            return;
        }

        let run_num = self.next_run_num();

        match daemon_request(
            &self.config,
            DaemonRequest::RunAgent {
                id: agent_name.clone(),
                input: Some(input.clone()),
            },
        )
        .await
        {
            Ok(DaemonResponse::ExecutionStarted { execution_id }) => {
                let msg = ChatMsg {
                    run_num,
                    input: input.clone(),
                    response: None,
                    status: ExecStatus::Running,
                    started_at: chrono::Utc::now().to_rfc3339(),
                    duration_secs: None,
                };
                let msgs = self.chat.entry(agent_name.clone()).or_default();
                let msg_idx = msgs.len();
                msgs.push(msg);

                self.pending = Some(Pending {
                    exec_id: execution_id,
                    agent_name,
                    msg_idx,
                });
                self.composer_input.clear();
                self.scroll = u16::MAX;
            }
            Ok(DaemonResponse::Error { message }) => self.set_status(&format!("✗ {}", message)),
            Err(e) => self.set_status(&format!("✗ {}", e)),
            _ => {}
        }
    }

    fn set_status(&mut self, msg: &str) {
        self.status_msg = Some((msg.to_string(), Instant::now()));
    }

    fn agent_next(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let next = (self.selected_agent + 1).min(self.agents.len() - 1);
        self.select_agent(next);
    }

    fn agent_prev(&mut self) {
        if self.selected_agent == 0 {
            return;
        }
        self.select_agent(self.selected_agent - 1);
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run_tui(config: AppConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

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

        // Expire status messages
        if let Some((_, t)) = &app.status_msg {
            if t.elapsed() > Duration::from_secs(4) {
                app.status_msg = None;
            }
        }

        // Periodic refresh or pending poll
        if app.last_refresh.elapsed() >= REFRESH || app.pending.is_some() {
            app.poll_pending().await;
            if app.last_refresh.elapsed() >= REFRESH {
                app.refresh().await;
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

async fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    use KeyCode::*;
    match (&app.focus, code, mods) {
        // Global quit
        (_, Char('c'), KeyModifiers::CONTROL) => return true,
        (Focus::Content, Char('q'), _) => return true,

        // Agent tab switching
        (Focus::Content, Right | Char('l'), _) => app.agent_next(),
        (Focus::Content, Left | Char('h'), _) => app.agent_prev(),

        // Sub-tab switching
        (Focus::Content, Char('1'), _) => {
            app.agent_tab = AgentTab::Chat;
            app.scroll = u16::MAX;
        }
        (Focus::Content, Char('2'), _) => {
            app.agent_tab = AgentTab::Runs;
            app.scroll = 0;
        }
        (Focus::Content, Char('3'), _) => {
            app.agent_tab = AgentTab::Config;
            app.scroll = 0;
        }

        // Scroll
        (Focus::Content, Down | Char('j'), _) => {
            app.scroll = app.scroll.saturating_add(1)
        }
        (Focus::Content, Up | Char('k'), _) => {
            app.scroll = app.scroll.saturating_sub(1)
        }
        (Focus::Content, Char('g'), _) => app.scroll = 0,
        (Focus::Content, Char('G'), _) => app.scroll = u16::MAX,

        // Enter composer
        (Focus::Content, Tab | Char('i'), _)
            if app.agent_tab == AgentTab::Chat =>
        {
            app.focus = Focus::Composer;
        }

        // Leave composer
        (Focus::Composer, Esc, _) => app.focus = Focus::Content,

        // Composer input
        (Focus::Composer, Char(c), _) => app.composer_input.push(c),
        (Focus::Composer, Backspace, _) => {
            app.composer_input.pop();
        }
        (Focus::Composer, Enter, _) => app.send_message().await,

        _ => {}
    }
    false
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let has_composer = app.agent_tab == AgentTab::Chat;

    let rows = if has_composer {
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(3),
        ])
        .split(area)
    } else {
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(0),
        ])
        .split(area)
    };

    render_topbar(f, app, rows[0]);
    render_agent_tabs(f, app, rows[1]);
    render_sub_tabs(f, app, rows[2]);

    match app.agent_tab {
        AgentTab::Chat => render_chat(f, app, rows[3]),
        AgentTab::Runs => render_runs(f, app, rows[3]),
        AgentTab::Config => render_config(f, app, rows[3]),
    }

    if has_composer && rows[4].height > 0 {
        render_composer(f, app, rows[4]);
    }
}

// ── Topbar ─────────────────────────────────────────────────────────────────────

fn render_topbar(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(area);

    let ver = if app.daemon_version.is_empty() {
        "—".to_string()
    } else {
        app.daemon_version.clone()
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("agenta ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(format!("v{ver}"), Style::default().fg(Color::DarkGray)),
        ])),
        cols[0],
    );

    let (dot, col) = if app.daemon_ok { ("●", Color::Green) } else { ("✗", Color::Red) };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("daemon {dot}  "), Style::default().fg(col)),
            Span::styled(
                "←/→:agent  1/2/3:tab  j/k:scroll  q:quit",
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .alignment(Alignment::Right),
        cols[1],
    );
}

// ── Agent tabs ─────────────────────────────────────────────────────────────────

fn render_agent_tabs(f: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = vec![];
    let max_width = area.width as usize;
    let mut used = 0usize;

    for (i, agent) in app.agents.iter().enumerate() {
        let name = agent["name"].as_str().unwrap_or("?");
        let short = abbrev(name);
        let status = agent["status"].as_str().unwrap_or("");

        let dot = match status {
            "running" | "Running" => Span::styled("▶ ", Style::default().fg(Color::Green)),
            "failed"  | "Failed"  => Span::styled("✗ ", Style::default().fg(Color::Red)),
            _                     => Span::styled("● ", Style::default().fg(Color::DarkGray)),
        };

        // Width estimate: 2(dot) + name + 2(padding) + 2(separator)
        let tab_width = 2 + short.len() + 4;
        if used + tab_width + 4 > max_width && i < app.agents.len() - 1 {
            spans.push(Span::styled("···", Style::default().fg(Color::DarkGray)));
            break;
        }

        if i == app.selected_agent {
            spans.push(Span::styled(
                " ",
                Style::default().bg(Color::DarkGray),
            ));
            spans.push(dot);
            spans.push(Span::styled(
                short,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                "  ",
                Style::default().bg(Color::DarkGray),
            ));
        } else {
            spans.push(Span::styled("  ", Style::default()));
            spans.push(dot);
            spans.push(Span::styled(short, Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled("  ", Style::default()));
        }

        used += tab_width;
    }

    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(Color::from_u32(0x161616))),
        area,
    );
}

// ── Sub-tabs ───────────────────────────────────────────────────────────────────

fn render_sub_tabs(f: &mut Frame, app: &App, area: Rect) {
    let short = app.active_short();
    let model = app.active_agent()
        .and_then(|a| a["model"].as_str())
        .unwrap_or("—");

    let tab_style = |t: AgentTab| -> Style {
        if app.agent_tab == t {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    };

    let pending_indicator = if app.pending.is_some() {
        Span::styled(" ▶", Style::default().fg(Color::Yellow))
    } else {
        Span::raw("")
    };

    let line = Line::from(vec![
        Span::styled(format!(" {}  · ", short), Style::default().fg(Color::DarkGray)),
        Span::styled("[1] chat  ", tab_style(AgentTab::Chat)),
        Span::styled("[2] runs  ", tab_style(AgentTab::Runs)),
        Span::styled("[3] config  ", tab_style(AgentTab::Config)),
        Span::styled(
            format!("  {}", model),
            Style::default().fg(Color::DarkGray),
        ),
        pending_indicator,
    ]);

    f.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::from_u32(0x111111))),
        area,
    );
}

// ── Chat view ──────────────────────────────────────────────────────────────────

fn render_chat(f: &mut Frame, app: &App, area: Rect) {
    let msgs = app.active_chat();
    let short = app.active_short();

    if msgs.is_empty() {
        let hint = format!(
            "  no conversation yet\n  press i or Tab to start chatting with {}",
            short
        );
        f.render_widget(
            Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let mut lines: Vec<Line> = vec![];

    for msg in msgs {
        // Run separator
        let ts = fmt_ts(&msg.started_at);
        lines.push(Line::from(Span::styled(
            format!("  ── run #{} · {} ──", msg.run_num, ts),
            Style::default().fg(Color::from_u32(0x333333)),
        )));
        lines.push(Line::from(""));

        // User message
        lines.push(Line::from(vec![
            Span::styled("  you", Style::default().fg(Color::DarkGray)),
            Span::styled(" › ", Style::default().fg(Color::DarkGray)),
            Span::styled(msg.input.clone(), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(""));

        // Agent response
        match &msg.status {
            ExecStatus::Running => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {}", short),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                    Span::styled("thinking...", Style::default().fg(Color::DarkGray)),
                ]));
                lines.push(Line::from(
                    Span::styled("  ▶ running", Style::default().fg(Color::Yellow)),
                ));
            }
            ExecStatus::Completed => {
                if let Some(resp) = &msg.response {
                    for (i, part) in resp.lines().enumerate() {
                        if i == 0 {
                            lines.push(Line::from(vec![
                                Span::styled(
                                    format!("  {}", short),
                                    Style::default().fg(Color::Cyan),
                                ),
                                Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                                Span::styled(part.to_string(), Style::default().fg(Color::White)),
                            ]));
                        } else {
                            let indent = " ".repeat(short.len() + 6);
                            lines.push(Line::from(Span::styled(
                                format!("{}{}", indent, part),
                                Style::default().fg(Color::White),
                            )));
                        }
                    }
                }
                lines.push(Line::from(
                    Span::styled("  ✓ completed", Style::default().fg(Color::from_u32(0x3b6d11))),
                ));
            }
            ExecStatus::Failed(err) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {}", short),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("failed: {}", err),
                        Style::default().fg(Color::Red),
                    ),
                ]));
                lines.push(Line::from(
                    Span::styled("  ✗ failed", Style::default().fg(Color::Red)),
                ));
            }
        }
        lines.push(Line::from(""));
    }

    // Auto-scroll to bottom: clamp scroll to max possible
    let total = lines.len() as u16;
    let visible = area.height;
    let max_scroll = total.saturating_sub(visible);
    let scroll = app.scroll.min(max_scroll);

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
}

// ── Runs view ──────────────────────────────────────────────────────────────────

fn render_runs(f: &mut Frame, app: &App, area: Rect) {
    let runs = app.runs_for_active();

    if runs.is_empty() {
        f.render_widget(
            Paragraph::new("  no runs yet").style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let mut lines: Vec<Line> = vec![];

    for (i, e) in runs.iter().enumerate() {
        let status = e["status"].as_str().unwrap_or("?");
        let ts = e["started_at"]
            .as_str()
            .map(fmt_ts)
            .unwrap_or_else(|| "—".to_string());
        let input_preview: String = e["input"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(50)
            .collect();
        let output_preview: String = e["output"]
            .as_str()
            .map(|o| extract_task_complete(o))
            .unwrap_or_default()
            .chars()
            .take(60)
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
            Span::styled(format!("  {:<5}", num), Style::default().fg(Color::DarkGray)),
            Span::styled(badge, Style::default().fg(badge_color)),
            Span::raw("  "),
            Span::styled(ts, Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(vec![
            Span::raw("        "),
            Span::styled(
                format!("in  › {}", input_preview),
                Style::default().fg(Color::White),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw("        "),
            Span::styled(
                if output_preview.is_empty() {
                    "out › —".to_string()
                } else {
                    format!("out › {}", output_preview)
                },
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::from(""));
    }

    let total = lines.len() as u16;
    let max_scroll = total.saturating_sub(area.height);
    let scroll = app.scroll.min(max_scroll);

    f.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)),
        area,
    );
}

// ── Config view ────────────────────────────────────────────────────────────────

fn render_config(f: &mut Frame, app: &App, area: Rect) {
    let agent = match app.active_agent() {
        Some(a) => a,
        None => {
            f.render_widget(
                Paragraph::new("  no agent selected").style(Style::default().fg(Color::DarkGray)),
                area,
            );
            return;
        }
    };

    let fields: &[(&str, &str)] = &[
        ("name",     "name"),
        ("model",    "model"),
        ("provider", "provider"),
        ("mode",     "execution_mode"),
        ("schedule", "schedule"),
        ("memory",   "memory_enabled"),
        ("deep",     "deep_agent"),
        ("status",   "status"),
    ];

    let mut lines: Vec<Line> = vec![Line::from("")];

    for (label, key) in fields {
        let val = match &agent[key] {
            serde_json::Value::Null | serde_json::Value::Bool(false) => {
                Span::styled("—", Style::default().fg(Color::DarkGray))
            }
            serde_json::Value::Bool(true) => {
                Span::styled("yes", Style::default().fg(Color::Green))
            }
            serde_json::Value::String(s) if s.is_empty() => {
                Span::styled("—", Style::default().fg(Color::DarkGray))
            }
            serde_json::Value::String(s) => {
                Span::styled(s.clone(), Style::default().fg(Color::White))
            }
            other => Span::styled(other.to_string(), Style::default().fg(Color::White)),
        };

        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:12}", format!("{}:", label)),
                Style::default().fg(Color::DarkGray),
            ),
            val,
        ]));
    }

    // Tool count
    let tool_count = app.run_count_for(&app.active_name());
    lines.push(Line::from(vec![
        Span::styled("  runs:       ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            tool_count.to_string(),
            Style::default().fg(Color::White),
        ),
    ]));

    // System prompt preview
    if let Some(prompt) = agent["system_prompt"].as_str() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  prompt:",
            Style::default().fg(Color::DarkGray),
        )));
        for chunk in prompt.chars().collect::<Vec<_>>().chunks(60) {
            let s: String = chunk.iter().collect();
            lines.push(Line::from(Span::styled(
                format!("    {}", s),
                Style::default().fg(Color::from_u32(0x555555)),
            )));
        }
    }

    f.render_widget(
        Paragraph::new(lines).scroll((app.scroll, 0)),
        area,
    );
}

// ── Composer ───────────────────────────────────────────────────────────────────

fn render_composer(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Composer;
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let short = app.active_short();
    let model = app.active_agent()
        .and_then(|a| a["model"].as_str())
        .unwrap_or("—");

    f.render_widget(
        Block::default()
            .borders(Borders::TOP)
            .border_style(border),
        area,
    );

    let inner = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

    // Status / context line
    let context = if let Some((msg, _)) = &app.status_msg {
        let color = if msg.starts_with('✗') { Color::Red } else { Color::Green };
        Line::from(Span::styled(format!("  {}", msg), Style::default().fg(color)))
    } else if app.pending.is_some() {
        Line::from(Span::styled(
            format!("  ▶ waiting for {} to respond...", short),
            Style::default().fg(Color::Yellow),
        ))
    } else {
        Line::from(vec![
            Span::styled(
                format!("  talking to "),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                short.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {}", model),
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };
    f.render_widget(Paragraph::new(context), rows[0]);

    // Input line
    let cursor = if focused { "▌" } else { "" };
    let input_line = Line::from(vec![
        Span::styled("  › ", Style::default().fg(Color::Cyan)),
        Span::styled(app.composer_input.clone(), Style::default().fg(Color::White)),
        Span::styled(cursor, Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(input_line), rows[1]);
}
