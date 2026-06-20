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
const TYPEWRITER_SPEED: usize = 8; // chars revealed per tick
const STOP_WORDS: &[&str] = &["and", "for", "of", "the", "a", "an", "to", "in"];
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SIDEBAR_W: u16 = 22;

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
    if ts.len() < 16 {
        return ts.to_string();
    }
    let date = &ts[..10];
    let time = &ts[11..16];
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
    text.trim().to_string()
}

fn strip_timestamp(line: &str) -> &str {
    let s = line.trim();
    if s.starts_with('[') {
        if let Some(pos) = s.find("] ") {
            return &s[pos + 2..];
        }
    }
    s
}

fn is_meaningful_log(line: &str) -> bool {
    let s = strip_timestamp(line);
    if s.is_empty() { return false; }
    // Skip noisy internal lines
    !s.starts_with("Starting agent")
        && !s.starts_with("Agent loop")
        && !s.starts_with("Iteration ")
        && !s.starts_with("TASK_COMPLETE")
        && !s.starts_with('{')
        && !s.starts_with('[')
        && !s.contains("execution_id")
        && s.len() > 3
}

// ── Robot logo (box-drawing, clean) ──────────────────────────────────────────

fn robot_logo<'a>() -> Vec<Line<'a>> {
    let f = Style::default().fg(Color::Rgb(90, 120, 145)); // frame
    let e = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD); // eyes
    let g = Style::default().fg(Color::Rgb(55, 80, 95));  // grill
    let h = Style::default().fg(Color::Green);             // heart
    let b = Style::default().fg(Color::Yellow);            // bitcoin
    let a = Style::default().fg(Color::Red);               // antenna

    // Each line is padded to fit the sidebar
    vec![
        Line::from(vec![
            Span::raw("       "),
            Span::styled("●", a),
        ]),
        Line::from(vec![Span::styled("  ┌─────────┐", f)]),
        Line::from(vec![
            Span::styled("  │ ", f),
            Span::styled("◉", e),
            Span::styled("     ", f),
            Span::styled("◉", e),
            Span::styled(" │", f),
        ]),
        Line::from(vec![
            Span::styled("  │  ", f),
            Span::styled("─────", g),
            Span::styled("  │", f),
        ]),
        Line::from(vec![Span::styled("  └─────────┘", f)]),
        Line::from(vec![Span::styled("     │   │   ", f)]),
        Line::from(vec![Span::styled("  ┌─────────┐", f)]),
        Line::from(vec![
            Span::styled("  │  ", f),
            Span::styled("♥", h),
            Span::styled("   ", f),
            Span::styled("₿", b),
            Span::styled("  │", f),
        ]),
        Line::from(vec![Span::styled("  └─────────┘", f)]),
    ]
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
    typewriter_pos: usize, // how many chars of response to reveal
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
    spinner_tick: usize,
    stream_lines: Vec<String>, // live log lines while pending
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
            spinner_tick: 0,
            stream_lines: vec![],
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
            None => {
                self.stream_lines.clear();
                return;
            }
        };

        self.spinner_tick = self.spinner_tick.wrapping_add(1);

        // Poll logs for live streaming display
        if let Ok(DaemonResponse::ExecutionLog { lines }) = daemon_request(
            &self.config,
            DaemonRequest::GetLogs {
                agent_id: pending.1.clone(),
                execution_id: Some(pending.0.clone()),
                lines: 30,
            },
        )
        .await
        {
            self.stream_lines = lines.iter()
                .filter(|l| is_meaningful_log(l))
                .map(|l| strip_timestamp(l).chars().take(70).collect::<String>())
                .collect();

            // Check if TASK_COMPLETE appeared in logs before execution record updates
            let tc = lines.iter().rev().find_map(|l| {
                let s = strip_timestamp(l);
                if let Some(pos) = s.find("TASK_COMPLETE:") {
                    let after = s[pos + "TASK_COMPLETE:".len()..].trim();
                    if !after.is_empty() { return Some(after.to_string()); }
                }
                None
            });
            if let Some(response) = tc {
                if let Some(msgs) = self.chat.get_mut(&pending.1) {
                    if let Some(msg) = msgs.get_mut(pending.2) {
                        if msg.status == ExecStatus::Running {
                            msg.response = Some(response);
                            // keep Running so typewriter starts; mark Completed below
                        }
                    }
                }
            }
        }

        // Check execution status
        if let Ok(DaemonResponse::ExecutionResult { result }) =
            daemon_request(&self.config, DaemonRequest::GetExecution { id: pending.0.clone() }).await
        {
            let status = result["status"].as_str().unwrap_or("running");
            match status {
                "completed" => {
                    let raw = result["output"].as_str().unwrap_or("").to_string();
                    let response = extract_task_complete(&raw);
                    if let Some(msgs) = self.chat.get_mut(&pending.1) {
                        if let Some(msg) = msgs.get_mut(pending.2) {
                            msg.response = Some(response);
                            msg.status = ExecStatus::Completed;
                            msg.typewriter_pos = 0; // start typewriter reveal
                        }
                    }
                    self.pending = None;
                    self.stream_lines.clear();
                }
                "failed" | "cancelled" => {
                    let err = result["error"].as_str().unwrap_or("unknown error").to_string();
                    if let Some(msgs) = self.chat.get_mut(&pending.1) {
                        if let Some(msg) = msgs.get_mut(pending.2) {
                            msg.response = Some(format!("failed: {}", err));
                            msg.status = ExecStatus::Failed(err);
                        }
                    }
                    self.pending = None;
                    self.stream_lines.clear();
                }
                _ => {}
            }
        }
    }

    fn advance_typewriter(&mut self) {
        let name = self.active_name();
        if let Some(msgs) = self.chat.get_mut(&name) {
            for msg in msgs.iter_mut() {
                if msg.status == ExecStatus::Completed {
                    if let Some(resp) = &msg.response {
                        let len = resp.len();
                        if msg.typewriter_pos < len {
                            msg.typewriter_pos =
                                (msg.typewriter_pos + TYPEWRITER_SPEED).min(len);
                        }
                    }
                }
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
                    typewriter_pos: 0,
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
        if self.agents.is_empty() { return; }
        let next = (self.selected_agent + 1).min(self.agents.len() - 1);
        self.select_agent(next);
    }

    fn agent_prev(&mut self) {
        if self.selected_agent == 0 { return; }
        self.select_agent(self.selected_agent - 1);
    }

    fn total_runs(&self) -> usize {
        self.executions.len()
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
        app.advance_typewriter();
        terminal.draw(|f| render(f, &mut app))?;

        if event::poll(POLL)? {
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

        if app.pending.is_some() {
            app.poll_pending().await;
        }

        if app.last_refresh.elapsed() >= REFRESH {
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
        (Focus::Content, Char('q'), _) => return true,

        (Focus::Content, Right | Char('l'), _) => app.agent_next(),
        (Focus::Content, Left | Char('h'), _) => app.agent_prev(),

        (Focus::Content, Char('1'), _) => { app.agent_tab = AgentTab::Chat; app.scroll = u16::MAX; }
        (Focus::Content, Char('2'), _) => { app.agent_tab = AgentTab::Runs; app.scroll = 0; }
        (Focus::Content, Char('3'), _) => { app.agent_tab = AgentTab::Config; app.scroll = 0; }

        (Focus::Content, Down | Char('j'), _) => { app.scroll = app.scroll.saturating_add(1); }
        (Focus::Content, Up | Char('k'), _)   => { app.scroll = app.scroll.saturating_sub(1); }
        (Focus::Content, Char('g'), _) => app.scroll = 0,
        (Focus::Content, Char('G'), _) => app.scroll = u16::MAX,

        (Focus::Content, Tab | Char('i'), _) if app.agent_tab == AgentTab::Chat => {
            app.focus = Focus::Composer;
        }

        (Focus::Composer, Esc, _) => app.focus = Focus::Content,
        (Focus::Composer, Char(c), _) => app.composer_input.push(c),
        (Focus::Composer, Backspace, _) => { app.composer_input.pop(); }
        (Focus::Composer, Enter, _) => app.send_message().await,

        _ => {}
    }
    false
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let has_composer = app.agent_tab == AgentTab::Chat;
    let composer_h = if has_composer { 5 } else { 0 };

    let rows = Layout::vertical([
        Constraint::Length(1),            // topbar
        Constraint::Length(1),            // agent tabs
        Constraint::Length(1),            // sub-tabs
        Constraint::Fill(1),              // content
        Constraint::Length(composer_h),   // composer
    ])
    .split(area);

    render_topbar(f, app, rows[0]);
    render_agent_tabs(f, app, rows[1]);
    render_sub_tabs(f, app, rows[2]);

    // Split content: left sidebar + main
    let [sidebar_area, main_area] = Layout::horizontal([
        Constraint::Length(SIDEBAR_W),
        Constraint::Fill(1),
    ])
    .areas(rows[3]);

    match app.agent_tab {
        AgentTab::Chat   => render_chat(f, app, main_area),
        AgentTab::Runs   => render_runs(f, app, main_area),
        AgentTab::Config => render_config(f, app, main_area),
    }

    render_sidebar(f, app, sidebar_area);

    if has_composer && rows[4].height > 0 {
        render_composer(f, app, rows[4]);
    }
}

// ── Topbar ─────────────────────────────────────────────────────────────────────

fn render_topbar(f: &mut Frame, app: &App, area: Rect) {
    let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(area);

    let ver = if app.daemon_version.is_empty() { "—".to_string() } else { app.daemon_version.clone() };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("agenta ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(format!("v{ver}"), Style::default().fg(Color::DarkGray)),
        ])),
        left,
    );

    let (dot, col) = if app.daemon_ok { ("●", Color::Green) } else { ("✗", Color::Red) };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("daemon {dot}  "), Style::default().fg(col)),
            Span::styled("←/→:agent  1/2/3:tab  j/k:scroll  q:quit", Style::default().fg(Color::DarkGray)),
        ]))
        .alignment(Alignment::Right),
        right,
    );
}

// ── Agent tabs ─────────────────────────────────────────────────────────────────

fn render_agent_tabs(f: &mut Frame, app: &App, area: Rect) {
    let max_width = area.width as usize;
    let mut spans: Vec<Span> = vec![];
    let mut used = 0usize;

    for (i, agent) in app.agents.iter().enumerate() {
        let name = agent["name"].as_str().unwrap_or("?");
        let short = abbrev(name);
        let status = agent["status"].as_str().unwrap_or("");

        let (dot_str, dot_color) = match status {
            "running" | "Running" => ("▶ ", Color::Green),
            "failed"  | "Failed"  => ("✗ ", Color::Red),
            _                     => ("● ", Color::DarkGray),
        };

        let tab_width = 2 + 2 + short.len() + 2;
        if used + tab_width + 4 > max_width && i < app.agents.len() - 1 {
            spans.push(Span::styled("···", Style::default().fg(Color::DarkGray)));
            break;
        }

        if i == app.selected_agent {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(dot_str, Style::default().fg(Color::Cyan)));
            spans.push(Span::styled(
                short,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ));
            spans.push(Span::raw("  "));
        } else {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(dot_str, Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(short, Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw("  "));
        }

        used += tab_width;
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::from_u32(0x161616))),
        area,
    );
}

// ── Sub-tabs ───────────────────────────────────────────────────────────────────

fn render_sub_tabs(f: &mut Frame, app: &App, area: Rect) {
    let short = app.active_short();
    let model = app.active_agent().and_then(|a| a["model"].as_str()).unwrap_or("—");

    let tab_style = |t: AgentTab| -> Style {
        if app.agent_tab == t {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    };

    let pending_indicator = if app.pending.is_some() {
        let spin = SPINNER[app.spinner_tick % SPINNER.len()];
        Span::styled(format!(" {}", spin), Style::default().fg(Color::Yellow))
    } else {
        Span::raw("")
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {}  · ", short), Style::default().fg(Color::DarkGray)),
            Span::styled("[1] chat  ", tab_style(AgentTab::Chat)),
            Span::styled("[2] runs  ", tab_style(AgentTab::Runs)),
            Span::styled("[3] config  ", tab_style(AgentTab::Config)),
            Span::styled(format!("  {}", model), Style::default().fg(Color::DarkGray)),
            pending_indicator,
        ]))
        .style(Style::default().bg(Color::from_u32(0x111111))),
        area,
    );
}

// ── Chat view ──────────────────────────────────────────────────────────────────

fn render_chat(f: &mut Frame, app: &App, area: Rect) {
    let msgs = app.active_chat();
    let short = app.active_short();

    if msgs.is_empty() {
        f.render_widget(
            Paragraph::new(format!(
                "\n  no conversation yet\n  press i or Tab to chat with {}",
                short
            ))
            .style(Style::default().fg(Color::DarkGray)),
            area,
        );
        return;
    }

    let mut lines: Vec<Line> = vec![];

    for (msg_i, msg) in msgs.iter().enumerate() {
        let ts = fmt_ts(&msg.started_at);
        lines.push(Line::from(Span::styled(
            format!("  ── run #{} · {} ──", msg.run_num, ts),
            Style::default().fg(Color::from_u32(0x2a2a2a)),
        )));
        lines.push(Line::from(""));

        // User message
        lines.push(Line::from(vec![
            Span::styled("  you", Style::default().fg(Color::DarkGray)),
            Span::styled(" › ", Style::default().fg(Color::DarkGray)),
            Span::styled(msg.input.clone(), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(""));

        match &msg.status {
            ExecStatus::Running => {
                let spin = SPINNER[app.spinner_tick % SPINNER.len()];
                // Show latest stream lines
                let stream_slice = {
                    let len = app.stream_lines.len();
                    &app.stream_lines[len.saturating_sub(4)..]
                };
                for sline in stream_slice {
                    lines.push(Line::from(Span::styled(
                        format!("    {}", sline),
                        Style::default().fg(Color::from_u32(0x3a3a3a)),
                    )));
                }
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}", short), Style::default().fg(Color::Cyan)),
                    Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{} thinking...", spin),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                lines.push(Line::from(Span::styled("  ▶ running", Style::default().fg(Color::Yellow))));
            }
            ExecStatus::Completed => {
                if let Some(resp) = &msg.response {
                    let is_latest = msg_i == msgs.len() - 1;
                    // Typewriter: reveal up to typewriter_pos chars if this is the latest
                    let display: &str = if is_latest && msg.typewriter_pos < resp.len() {
                        &resp[..msg.typewriter_pos]
                    } else {
                        resp.as_str()
                    };
                    let is_animating = is_latest && msg.typewriter_pos < resp.len();

                    for (i, part) in display.lines().enumerate() {
                        if i == 0 {
                            let mut spans = vec![
                                Span::styled(format!("  {}", short), Style::default().fg(Color::Cyan)),
                                Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                                Span::styled(part.to_string(), Style::default().fg(Color::White)),
                            ];
                            if is_animating && i == display.lines().count() - 1 {
                                spans.push(Span::styled("▌", Style::default().fg(Color::Cyan)));
                            }
                            lines.push(Line::from(spans));
                        } else {
                            let indent = " ".repeat(short.len() + 6);
                            let mut spans = vec![Span::styled(
                                format!("{}{}", indent, part),
                                Style::default().fg(Color::White),
                            )];
                            if is_animating && i == display.lines().count() - 1 {
                                spans.push(Span::styled("▌", Style::default().fg(Color::Cyan)));
                            }
                            lines.push(Line::from(spans));
                        }
                    }
                }
                lines.push(Line::from(Span::styled(
                    "  ✓ completed",
                    Style::default().fg(Color::Rgb(59, 109, 17)),
                )));
            }
            ExecStatus::Failed(err) => {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}", short), Style::default().fg(Color::Cyan)),
                    Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("failed: {}", err), Style::default().fg(Color::Red)),
                ]));
                lines.push(Line::from(Span::styled("  ✗ failed", Style::default().fg(Color::Red))));
            }
        }
        lines.push(Line::from(""));
    }

    let total = lines.len() as u16;
    let max_scroll = total.saturating_sub(area.height);
    let scroll = app.scroll.min(max_scroll);

    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)),
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
        let ts = e["started_at"].as_str().map(fmt_ts).unwrap_or_else(|| "—".to_string());
        let input_preview: String = e["input"].as_str().unwrap_or("").chars().take(55).collect();
        let output_preview: String = e["output"].as_str()
            .map(|o| extract_task_complete(o))
            .unwrap_or_default()
            .chars()
            .take(65)
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
            Span::styled(format!("in  › {}", input_preview), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(vec![
            Span::raw("        "),
            Span::styled(
                if output_preview.is_empty() { "out › —".to_string() } else { format!("out › {}", output_preview) },
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        lines.push(Line::from(""));
    }

    let max_scroll = (lines.len() as u16).saturating_sub(area.height);
    f.render_widget(Paragraph::new(lines).scroll((app.scroll.min(max_scroll), 0)), area);
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
            serde_json::Value::Bool(true) => Span::styled("yes", Style::default().fg(Color::Green)),
            serde_json::Value::String(s) if s.is_empty() => {
                Span::styled("—", Style::default().fg(Color::DarkGray))
            }
            serde_json::Value::String(s) => Span::styled(s.clone(), Style::default().fg(Color::White)),
            other => Span::styled(other.to_string(), Style::default().fg(Color::White)),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {:12}", format!("{}:", label)), Style::default().fg(Color::DarkGray)),
            val,
        ]));
    }

    let runs = app.run_count_for(&app.active_name());
    lines.push(Line::from(vec![
        Span::styled("  runs:       ", Style::default().fg(Color::DarkGray)),
        Span::styled(runs.to_string(), Style::default().fg(Color::White)),
    ]));

    if let Some(prompt) = agent["system_prompt"].as_str() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  prompt:", Style::default().fg(Color::DarkGray))));
        for chunk in prompt.chars().collect::<Vec<_>>().chunks(58) {
            let s: String = chunk.iter().collect();
            lines.push(Line::from(Span::styled(
                format!("    {}", s),
                Style::default().fg(Color::from_u32(0x444444)),
            )));
        }
    }

    f.render_widget(Paragraph::new(lines).scroll((app.scroll, 0)), area);
}

// ── Right sidebar ──────────────────────────────────────────────────────────────

fn render_sidebar(f: &mut Frame, app: &App, area: Rect) {
    if area.width < 14 { return; }

    // Subtle right border dividing sidebar from main content
    f.render_widget(
        Block::default().borders(Borders::RIGHT).border_style(Style::default().fg(Color::from_u32(0x222222))),
        area,
    );

    let inner = Rect { width: area.width.saturating_sub(1), ..area };

    let mut lines: Vec<Line> = vec![Line::from("")];

    // Robot logo
    for logo_line in robot_logo() {
        lines.push(logo_line);
    }

    lines.push(Line::from(""));

    // "agenta" title
    lines.push(Line::from(Span::styled(
        " agenta",
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        " AI Agent Runtime",
        Style::default().fg(Color::DarkGray),
    )));

    lines.push(Line::from(""));

    // Version
    let ver = if app.daemon_version.is_empty() { "—".into() } else { format!("v{}", app.daemon_version) };
    lines.push(Line::from(vec![
        Span::styled(" version  ", Style::default().fg(Color::from_u32(0x444444))),
        Span::styled(ver, Style::default().fg(Color::DarkGray)),
    ]));

    // Daemon status
    let (dot, dcol) = if app.daemon_ok { ("●", Color::Green) } else { ("✗", Color::Red) };
    lines.push(Line::from(vec![
        Span::styled(" daemon   ", Style::default().fg(Color::from_u32(0x444444))),
        Span::styled(dot, Style::default().fg(dcol)),
    ]));

    // Agent count
    lines.push(Line::from(vec![
        Span::styled(" agents   ", Style::default().fg(Color::from_u32(0x444444))),
        Span::styled(app.agents.len().to_string(), Style::default().fg(Color::DarkGray)),
    ]));

    // Total runs
    lines.push(Line::from(vec![
        Span::styled(" runs     ", Style::default().fg(Color::from_u32(0x444444))),
        Span::styled(app.total_runs().to_string(), Style::default().fg(Color::DarkGray)),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

// ── Composer ───────────────────────────────────────────────────────────────────

fn render_composer(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Composer;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::from_u32(0x2a2a2a))
    };

    let short = app.active_short();
    let model = app.active_agent().and_then(|a| a["model"].as_str()).unwrap_or("—");

    f.render_widget(
        Block::default().borders(Borders::TOP).border_style(border_style),
        area,
    );

    let inner = Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area };

    let [ctx_row, hint_row, _spacer, input_row] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    // Context / status line
    let context = if let Some((msg, _)) = &app.status_msg {
        let color = if msg.starts_with('✗') { Color::Red } else { Color::Green };
        Line::from(Span::styled(format!("  {}", msg), Style::default().fg(color)))
    } else if app.pending.is_some() {
        let spin = SPINNER[app.spinner_tick % SPINNER.len()];
        Line::from(vec![
            Span::styled(format!("  {} waiting for ", spin), Style::default().fg(Color::Yellow)),
            Span::styled(short.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" to respond...", Style::default().fg(Color::Yellow)),
        ])
    } else {
        Line::from(vec![
            Span::styled("  talking to ", Style::default().fg(Color::DarkGray)),
            Span::styled(short.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" · {}", model), Style::default().fg(Color::DarkGray)),
        ])
    };
    f.render_widget(Paragraph::new(context), ctx_row);

    // Log hint line
    if let Some(hint) = app.stream_lines.last() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("    {}", hint),
                Style::default().fg(Color::from_u32(0x3a3a3a)),
            ))),
            hint_row,
        );
    }

    // Input line
    let cursor = if focused { "▌" } else { "" };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  › ", Style::default().fg(Color::Cyan)),
            Span::styled(app.composer_input.clone(), Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ])),
        input_row,
    );
}
