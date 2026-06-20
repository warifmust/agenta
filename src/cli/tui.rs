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
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::core::{AppConfig, DaemonRequest, DaemonResponse};
use super::commands::daemon_request;

const REFRESH_SECS: Duration = Duration::from_secs(2);
const POLL_MS: Duration = Duration::from_millis(100);
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

fn short_ts(ts: &str) -> String {
    // "2026-06-19T10:01:23..." → "Jun 19 10:01"
    let t = if ts.len() > 16 { &ts[..16] } else { ts };
    t.replace('T', " ")
        .trim_start_matches(|c: char| c.is_ascii_digit() && false) // keep year for now
        .to_string()
}

// ── Focus / Tab ───────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum Focus {
    Sidebar,
    Main,
    Composer,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum MainTab {
    Execution,
    RawLogs,
}

// ── App state ─────────────────────────────────────────────────────────────────

struct App {
    config: AppConfig,
    agents: Vec<serde_json::Value>,
    executions: Vec<serde_json::Value>,
    log_lines: Vec<String>,
    daemon_ok: bool,
    daemon_version: String,
    sidebar_state: ListState,
    focus: Focus,
    tab: MainTab,
    log_scroll: u16,
    composer_input: String,
    composer_to_idx: usize,
    status_msg: Option<(String, Instant)>,
    last_refresh: Instant,
}

impl App {
    fn new(config: AppConfig) -> Self {
        let mut sidebar_state = ListState::default();
        sidebar_state.select(Some(0));
        Self {
            config,
            agents: vec![],
            executions: vec![],
            log_lines: vec![],
            daemon_ok: false,
            daemon_version: String::new(),
            sidebar_state,
            focus: Focus::Sidebar,
            tab: MainTab::Execution,
            log_scroll: 0,
            composer_input: String::new(),
            composer_to_idx: 0,
            status_msg: None,
            last_refresh: Instant::now() - Duration::from_secs(60),
        }
    }

    fn selected_agent(&self) -> Option<&serde_json::Value> {
        self.sidebar_state.selected().and_then(|i| self.agents.get(i))
    }

    fn selected_name(&self) -> String {
        self.selected_agent()
            .and_then(|a| a["name"].as_str())
            .unwrap_or("—")
            .to_string()
    }

    fn composer_targets(&self) -> Vec<String> {
        std::iter::once("DALANG".to_string())
            .chain(self.agents.iter().filter_map(|a| {
                a["name"].as_str().map(abbrev)
            }))
            .collect()
    }

    fn composer_target_label(&self) -> String {
        let targets = self.composer_targets();
        targets
            .get(self.composer_to_idx)
            .cloned()
            .unwrap_or_else(|| "DALANG".to_string())
    }

    fn composer_target_id(&self) -> String {
        if self.composer_to_idx == 0 {
            self.agents
                .iter()
                .find(|a| {
                    a["name"]
                        .as_str()
                        .map(|n| n.contains("dalang"))
                        .unwrap_or(false)
                })
                .and_then(|a| a["name"].as_str())
                .unwrap_or("dalang")
                .to_string()
        } else {
            self.agents
                .get(self.composer_to_idx - 1)
                .and_then(|a| a["name"].as_str())
                .unwrap_or("")
                .to_string()
        }
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
            let prev = self.sidebar_state.selected().unwrap_or(0);
            self.agents = agents;
            if !self.agents.is_empty() {
                self.sidebar_state
                    .select(Some(prev.min(self.agents.len() - 1)));
            }
        }

        if let Ok(DaemonResponse::ExecutionList { executions }) =
            daemon_request(&self.config, DaemonRequest::ListExecutions { limit: 40 }).await
        {
            self.executions = executions;
        }

        self.refresh_logs().await;
    }

    async fn refresh_logs(&mut self) {
        let name = self.selected_name();
        if name == "—" {
            return;
        }
        if let Ok(DaemonResponse::ExecutionLog { lines }) = daemon_request(
            &self.config,
            DaemonRequest::GetLogs {
                agent_id: name,
                execution_id: None,
                lines: 200,
            },
        )
        .await
        {
            self.log_lines = lines;
        }
    }

    fn sidebar_next(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let i = self.sidebar_state.selected().unwrap_or(0);
        self.sidebar_state
            .select(Some((i + 1).min(self.agents.len() - 1)));
        self.reset_main();
    }

    fn sidebar_prev(&mut self) {
        if self.agents.is_empty() {
            return;
        }
        let i = self.sidebar_state.selected().unwrap_or(0);
        self.sidebar_state.select(Some(i.saturating_sub(1)));
        self.reset_main();
    }

    fn reset_main(&mut self) {
        self.log_lines.clear();
        self.log_scroll = 0;
        self.last_refresh = Instant::now() - Duration::from_secs(60);
    }

    async fn send_composer(&mut self) {
        let input = self.composer_input.trim().to_string();
        if input.is_empty() {
            return;
        }
        let agent_id = self.composer_target_id();
        if agent_id.is_empty() {
            self.set_status("✗ no agent found for target");
            return;
        }
        match daemon_request(
            &self.config,
            DaemonRequest::RunAgent {
                id: agent_id.clone(),
                input: Some(input),
            },
        )
        .await
        {
            Ok(DaemonResponse::ExecutionStarted { execution_id }) => {
                let short = &execution_id[..execution_id.len().min(8)];
                self.set_status(&format!("▶ {} started ({})", agent_id, short));
            }
            Ok(DaemonResponse::Error { message }) => self.set_status(&format!("✗ {}", message)),
            Err(e) => self.set_status(&format!("✗ {}", e)),
            _ => {}
        }
        self.composer_input.clear();
        self.last_refresh = Instant::now() - Duration::from_secs(60);
    }

    fn set_status(&mut self, msg: &str) {
        self.status_msg = Some((msg.to_string(), Instant::now()));
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

        if event::poll(POLL_MS)? {
            if let Event::Key(key) = event::read()? {
                if handle_key(&mut app, key.code, key.modifiers).await {
                    break;
                }
            }
        }

        if let Some((_, t)) = &app.status_msg {
            if t.elapsed() > Duration::from_secs(4) {
                app.status_msg = None;
            }
        }

        if app.last_refresh.elapsed() >= REFRESH_SECS {
            app.refresh().await;
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

async fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    use KeyCode::*;
    match (&app.focus, code, mods) {
        (_, Char('c'), KeyModifiers::CONTROL) => return true,
        (Focus::Sidebar | Focus::Main, Char('q'), _) => return true,

        // Focus transitions
        (Focus::Sidebar, Tab | Right | Enter, _) => {
            app.focus = Focus::Main;
            app.log_scroll = 0;
        }
        (Focus::Main, Left | Esc, _) => app.focus = Focus::Sidebar,
        (Focus::Main, Tab, _) | (Focus::Main, Char('i'), _) => app.focus = Focus::Composer,
        (Focus::Composer, Esc, _) => app.focus = Focus::Sidebar,

        // Sidebar
        (Focus::Sidebar, Down | Char('j'), _) => app.sidebar_next(),
        (Focus::Sidebar, Up | Char('k'), _) => app.sidebar_prev(),

        // Main scroll + tab switch
        (Focus::Main, Down | Char('j'), _) => {
            app.log_scroll = app.log_scroll.saturating_add(1)
        }
        (Focus::Main, Up | Char('k'), _) => {
            app.log_scroll = app.log_scroll.saturating_sub(1)
        }
        (Focus::Main, Char('1'), _) => app.tab = MainTab::Execution,
        (Focus::Main, Char('2'), _) => app.tab = MainTab::RawLogs,

        // Composer input
        (Focus::Composer, Char(c), _) => app.composer_input.push(c),
        (Focus::Composer, Backspace, _) => {
            app.composer_input.pop();
        }
        (Focus::Composer, Enter, _) => app.send_composer().await,
        (Focus::Composer, Tab, _) => {
            let max = app.composer_targets().len().max(1);
            app.composer_to_idx = (app.composer_to_idx + 1) % max;
        }

        _ => {}
    }
    false
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(4),
    ])
    .split(area);

    render_topbar(f, app, rows[0]);

    let cols = Layout::horizontal([
        Constraint::Length(24),
        Constraint::Fill(1),
        Constraint::Length(26),
    ])
    .split(rows[1]);

    render_sidebar(f, app, cols[0]);
    render_center(f, app, cols[1]);
    render_right(f, app, cols[2]);
    render_composer(f, app, rows[2]);
}

// ── Topbar ─────────────────────────────────────────────────────────────────────

fn render_topbar(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).split(area);

    let ver = if app.daemon_version.is_empty() {
        "—".to_string()
    } else {
        app.daemon_version.clone()
    };
    let left = Line::from(vec![
        Span::styled(
            "agenta ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("v{ver}"), Style::default().fg(Color::DarkGray)),
    ]);

    let (dot, dot_color) = if app.daemon_ok {
        ("●", Color::Green)
    } else {
        ("✗", Color::Red)
    };
    let n = app.agents.len();
    let right = Line::from(vec![
        Span::styled(format!("daemon {dot}  "), Style::default().fg(dot_color)),
        Span::styled(
            format!("{n} agents  q:quit  tab:focus"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    f.render_widget(Paragraph::new(left), cols[0]);
    f.render_widget(
        Paragraph::new(right).alignment(Alignment::Right),
        cols[1],
    );
}

// ── Sidebar ────────────────────────────────────────────────────────────────────

fn status_dot(status: &str) -> Span<'static> {
    match status.to_lowercase().as_str() {
        "running" => Span::styled("▶", Style::default().fg(Color::Green)),
        "failed" | "error" => Span::styled("✗", Style::default().fg(Color::Red)),
        _ => Span::styled("●", Style::default().fg(Color::DarkGray)),
    }
}

fn mode_tag(mode: &str) -> Span<'static> {
    match mode.to_lowercase().as_str() {
        "scheduled" => Span::styled("sched", Style::default().fg(Color::Blue)),
        "continuous" => Span::styled("cont ", Style::default().fg(Color::Yellow)),
        _ => Span::styled("once ", Style::default().fg(Color::DarkGray)),
    }
}

fn render_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" agents ")
        .borders(Borders::ALL)
        .border_style(border);

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.agents.is_empty() {
        f.render_widget(
            Paragraph::new(if app.daemon_ok {
                "no agents"
            } else {
                "daemon offline\nagenta daemon start"
            })
            .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .agents
        .iter()
        .map(|a| {
            let name = a["name"].as_str().unwrap_or("?");
            let status = a["status"].as_str().unwrap_or("");
            let mode = a["execution_mode"].as_str().unwrap_or("");
            let short = abbrev(name);
            ListItem::new(Line::from(vec![
                status_dot(status),
                Span::raw(" "),
                Span::styled(
                    format!("{:<6} ", short),
                    Style::default().fg(Color::White),
                ),
                mode_tag(mode),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, inner, &mut app.sidebar_state);
}

// ── Center ─────────────────────────────────────────────────────────────────────

fn log_line_style(line: &str) -> Style {
    if line.contains("TOOL_CALL:") || line.contains("TOOL_CALL :") {
        Style::default().fg(Color::Yellow)
    } else if line.contains("TASK_COMPLETE:") {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else if line.to_lowercase().contains("error")
        || line.to_lowercase().contains("failed")
        || line.contains("✗")
    {
        Style::default().fg(Color::Red)
    } else if line.contains("→") || line.contains("->") || line.starts_with('{') {
        Style::default().fg(Color::Cyan)
    } else if line.starts_with('[') || line.starts_with("20") {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    }
}

fn render_center(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Main;
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let name = app.selected_name();
    let short = abbrev(&name);
    let model = app
        .selected_agent()
        .and_then(|a| a["model"].as_str())
        .unwrap_or("—");

    let block = Block::default()
        .title(format!(" {} · {} ", short, model))
        .borders(Borders::ALL)
        .border_style(border);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let tab_area = Rect {
        height: 1,
        ..inner
    };
    let content_area = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };

    // Tab bar
    let t1 = if app.tab == MainTab::Execution {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let t2 = if app.tab == MainTab::RawLogs {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let tab_line = Line::from(vec![
        Span::styled("[1] execution  ", t1),
        Span::styled("[2] raw logs  ", t2),
        Span::styled("j/k:scroll  ←:sidebar  i:compose", Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(tab_line), tab_area);

    if app.log_lines.is_empty() {
        f.render_widget(
            Paragraph::new(if app.daemon_ok {
                "select an agent and press → to view logs"
            } else {
                "daemon not running"
            })
            .style(Style::default().fg(Color::DarkGray)),
            content_area,
        );
        return;
    }

    let style_fn: fn(&str) -> Style = match app.tab {
        MainTab::Execution => log_line_style,
        MainTab::RawLogs => |_| Style::default().fg(Color::DarkGray),
    };

    let lines: Vec<Line> = app
        .log_lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), style_fn(l))))
        .collect();

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.log_scroll, 0)),
        content_area,
    );
}

// ── Right panel ────────────────────────────────────────────────────────────────

fn render_right(f: &mut Frame, app: &App, area: Rect) {
    let selected_name = app.selected_name();
    let short = abbrev(&selected_name);

    let block = Block::default()
        .title(format!(" runs · {} ", short))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let agent_runs: Vec<&serde_json::Value> = app
        .executions
        .iter()
        .filter(|e| {
            e["agent_name"]
                .as_str()
                .or_else(|| e["agent_id"].as_str())
                .map(|n| n == selected_name)
                .unwrap_or(false)
        })
        .take(25)
        .collect();

    if agent_runs.is_empty() {
        f.render_widget(
            Paragraph::new("no runs yet").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = agent_runs
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let status = e["status"].as_str().unwrap_or("?");
            let ts = e["created_at"]
                .as_str()
                .or_else(|| e["started_at"].as_str())
                .map(short_ts)
                .unwrap_or_else(|| "—".to_string());

            let (badge, badge_color) = match status {
                "completed" | "done" => ("✓", Color::Green),
                "running" => ("▶", Color::Yellow),
                "failed" | "error" => ("✗", Color::Red),
                _ => ("·", Color::DarkGray),
            };

            let preview = e["output"]
                .as_str()
                .map(|s| s.chars().take(20).collect::<String>())
                .unwrap_or_default();

            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("#{:<3}", i + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(badge, Style::default().fg(badge_color)),
                    Span::raw("  "),
                    Span::styled(ts, Style::default().fg(Color::DarkGray)),
                ]),
                Line::from(Span::styled(
                    format!("     {}", preview),
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();

    f.render_widget(List::new(items), inner);
}

// ── Composer ───────────────────────────────────────────────────────────────────

fn render_composer(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Composer;
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
        .border_style(border);

    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    // Status / To: line
    let header = if let Some((msg, _)) = &app.status_msg {
        let color = if msg.starts_with('✗') {
            Color::Red
        } else {
            Color::Green
        };
        Line::from(Span::styled(msg.clone(), Style::default().fg(color)))
    } else {
        let target = app.composer_target_label();
        Line::from(vec![
            Span::styled("to: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("[{} ▾]", target),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  tab:switch",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };
    f.render_widget(Paragraph::new(header), rows[0]);

    // Input line
    let cursor = if focused { "▌" } else { " " };
    let input_line = Line::from(vec![
        Span::styled("› ", Style::default().fg(Color::Cyan)),
        Span::styled(
            app.composer_input.clone(),
            Style::default().fg(Color::White),
        ),
        Span::styled(cursor, Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(input_line), rows[1]);

    // Hint
    let hint = if focused {
        "  enter:send  esc:cancel"
    } else {
        "  press tab to focus  ·  enter to send"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[2],
    );
}
