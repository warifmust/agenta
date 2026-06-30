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
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::core::{Agent, AppConfig, DaemonRequest, DaemonResponse};
use super::commands::{daemon_request, read_installed_tool};

const REFRESH: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);
const TYPEWRITER_SPEED: usize = 8;
const STOP_WORDS: &[&str] = &["and", "for", "of", "the", "a", "an", "to", "in"];
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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

fn strip_timestamp(line: &str) -> &str {
    let s = line.trim();
    if s.starts_with('[') {
        if let Some(pos) = s.find("] ") { return &s[pos + 2..]; }
    }
    s
}

fn is_meaningful_log(line: &str) -> bool {
    let s = strip_timestamp(line);
    if s.is_empty() { return false; }
    !s.starts_with("Starting agent")
        && !s.starts_with("Agent loop")
        && !s.starts_with("Iteration ")
        && !s.starts_with("TASK_COMPLETE")
        && !s.starts_with('{')
        && !s.starts_with('[')
        && !s.contains("execution_id")
        && s.len() > 3
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

fn infer_lang(handler: &str) -> &'static str {
    if handler.contains("python3") || handler.contains("python ") { "py" }
    else if handler.contains("node") { "js" }
    else { "sh" }
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

fn infer_provider(model: &str) -> &'static str {
    if model.starts_with("deepseek") { "deepseek" }
    else if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") { "openai" }
    else if model.contains("claude") { "anthropic" }
    else { "ollama" }
}

// ── Domain types ──────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone, Copy)]
enum Panel {
    Agents,
    Executions,
    Chat,
    Tools,
}

impl Panel {
    fn next(self) -> Self {
        match self {
            Panel::Agents     => Panel::Executions,
            Panel::Executions => Panel::Chat,
            Panel::Chat       => Panel::Tools,
            Panel::Tools      => Panel::Agents,
        }
    }
    fn prev(self) -> Self {
        match self {
            Panel::Agents     => Panel::Tools,
            Panel::Executions => Panel::Agents,
            Panel::Chat       => Panel::Executions,
            Panel::Tools      => Panel::Chat,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
enum AgentMode {
    List,
    View { agent: serde_json::Value },
    WizardName(String),
    WizardModel { name: String, model: String },
    EditModel { agent_id: String, agent_name: String, model: String },
    ConfirmDelete { agent_id: String, agent_name: String },
    AttachTool { agent_id: String, agent_name: String, input: String },
}

#[derive(Debug, PartialEq, Clone)]
enum ToolMode {
    List,
    WizardName(String),
    WizardPurpose { name: String, purpose: String },
    Generating { name: String },
    Done { summary: String },
    ConfirmDelete { tool_id: String, tool_name: String },
    PullName(String),
    PullAttach { name: String, input: String },
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum ChatMode {
    Viewing,
    Composing,
}

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
    typewriter_pos: usize,
}

struct Pending {
    exec_id: String,
    agent_name: String,
    msg_idx: usize,
}

struct ToolPending {
    exec_id: String,
    name: String,
}

// ── App state ─────────────────────────────────────────────────────────────────

struct App {
    config: AppConfig,
    agents: Vec<serde_json::Value>,
    executions: Vec<serde_json::Value>,
    tools: Vec<serde_json::Value>,
    chat: HashMap<String, Vec<ChatMsg>>,
    selected_agent: usize,
    selected_tool: usize,
    active_panel: Panel,
    agent_mode: AgentMode,
    chat_mode: ChatMode,
    tool_mode: ToolMode,
    composer_input: String,
    pending: Option<Pending>,
    tool_pending: Option<ToolPending>,
    stream_lines: Vec<String>,
    tool_stream_lines: Vec<String>,
    chat_scroll: u16,
    exec_scroll: u16,
    daemon_ok: bool,
    daemon_version: String,
    status_msg: Option<(String, Instant)>,
    last_refresh: Instant,
    spinner_tick: usize,
}

impl App {
    fn new(config: AppConfig) -> Self {
        Self {
            config,
            agents: vec![],
            executions: vec![],
            tools: vec![],
            chat: HashMap::new(),
            selected_agent: 0,
            selected_tool: 0,
            active_panel: Panel::Agents,
            agent_mode: AgentMode::List,
            chat_mode: ChatMode::Viewing,
            tool_mode: ToolMode::List,
            composer_input: String::new(),
            pending: None,
            tool_pending: None,
            stream_lines: vec![],
            tool_stream_lines: vec![],
            chat_scroll: 0,
            exec_scroll: 0,
            daemon_ok: false,
            daemon_version: String::new(),
            status_msg: None,
            last_refresh: Instant::now() - Duration::from_secs(60),
            spinner_tick: 0,
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

    fn next_run_num(&self) -> usize {
        let name = self.active_name();
        self.chat.get(&name).map(|v| v.len() + 1).unwrap_or(1)
    }

    // System agents are not in self.agents (filtered by daemon).
    // MIND is always addressed by its fixed name "MIND".
    fn tool_engineer_id() -> &'static str {
        "MIND"
    }

    fn set_status(&mut self, msg: &str) {
        self.status_msg = Some((msg.to_string(), Instant::now()));
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

        self.tools = merge_tools(
            load_installed_tools(),
            match daemon_request(&self.config, DaemonRequest::ListTools).await {
                Ok(DaemonResponse::ToolList { tools }) => tools,
                _ => vec![],
            },
        );
        if !self.tools.is_empty() {
            self.selected_tool = self.selected_tool.min(self.tools.len() - 1);
        }

        self.poll_pending().await;
        self.poll_tool_pending().await;
    }

    async fn poll_pending(&mut self) {
        let pending = match &self.pending {
            Some(p) => (p.exec_id.clone(), p.agent_name.clone(), p.msg_idx),
            None => { self.stream_lines.clear(); return; }
        };

        self.spinner_tick = self.spinner_tick.wrapping_add(1);

        if let Ok(DaemonResponse::ExecutionLog { lines }) = daemon_request(
            &self.config,
            DaemonRequest::GetLogs {
                agent_id: pending.1.clone(),
                execution_id: Some(pending.0.clone()),
                lines: 30,
            },
        ).await {
            self.stream_lines = lines.iter()
                .filter(|l| is_meaningful_log(l))
                .map(|l| strip_timestamp(l).chars().take(70).collect::<String>())
                .collect();

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
                        }
                    }
                }
            }
        }

        if let Ok(DaemonResponse::ExecutionResult { result }) =
            daemon_request(&self.config, DaemonRequest::GetExecution { id: pending.0.clone() }).await
        {
            match result["status"].as_str().unwrap_or("running") {
                "completed" => {
                    let raw = result["output"].as_str().unwrap_or("").to_string();
                    let response = extract_task_complete(&raw);
                    if let Some(msgs) = self.chat.get_mut(&pending.1) {
                        if let Some(msg) = msgs.get_mut(pending.2) {
                            msg.response = Some(response);
                            msg.status = ExecStatus::Completed;
                            msg.typewriter_pos = 0;
                        }
                    }
                    self.pending = None;
                    self.stream_lines.clear();
                    self.active_panel = Panel::Chat;
                    self.chat_mode = ChatMode::Composing;
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

    async fn poll_tool_pending(&mut self) {
        let tp = match &self.tool_pending {
            Some(p) => (p.exec_id.clone(), p.name.clone()),
            None => return,
        };
        let engineer_id = Self::tool_engineer_id().to_string();

        self.spinner_tick = self.spinner_tick.wrapping_add(1);

        if let Ok(DaemonResponse::ExecutionLog { lines }) = daemon_request(
            &self.config,
            DaemonRequest::GetLogs {
                agent_id: engineer_id.clone(),
                execution_id: Some(tp.0.clone()),
                lines: 30,
            },
        ).await {
            self.tool_stream_lines = lines.iter()
                .filter(|l| is_meaningful_log(l))
                .map(|l| strip_timestamp(l).chars().take(55).collect::<String>())
                .collect();
        }

        if let Ok(DaemonResponse::ExecutionResult { result }) =
            daemon_request(&self.config, DaemonRequest::GetExecution { id: tp.0.clone() }).await
        {
            match result["status"].as_str().unwrap_or("running") {
                "completed" => {
                    let raw = result["output"].as_str().unwrap_or("").to_string();
                    let summary = extract_task_complete(&raw);
                    self.tool_mode = ToolMode::Done { summary };
                    self.tool_pending = None;
                    self.tool_stream_lines.clear();
                    self.tools = merge_tools(load_installed_tools(), match daemon_request(&self.config, DaemonRequest::ListTools).await { Ok(DaemonResponse::ToolList { tools }) => tools, _ => vec![] });
                    if !self.tools.is_empty() {
                        self.selected_tool = self.selected_tool.min(self.tools.len() - 1);
                    }
                }
                "failed" | "cancelled" => {
                    let err = result["error"].as_str().unwrap_or("failed").to_string();
                    self.tool_mode = ToolMode::Done { summary: format!("✗ MIND failed: {}", err) };
                    self.tool_pending = None;
                    self.tool_stream_lines.clear();
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
                        let len = resp.chars().count();
                        if msg.typewriter_pos < len {
                            msg.typewriter_pos = (msg.typewriter_pos + TYPEWRITER_SPEED).min(len);
                        }
                    }
                }
            }
        }
    }

    async fn create_agent(&mut self, name: String, model: String) {
        let provider = infer_provider(&model).to_string();
        let agent = serde_json::json!({
            "name": name.trim(),
            "model": model.trim(),
            "provider": provider,
            "system_prompt": "",
            "execution_mode": "once",
            "status": "active",
            "memory_enabled": false,
            "deep_agent": false,
            "temperature": 0.7,
            "max_tokens": 4096
        });
        match daemon_request(&self.config, DaemonRequest::CreateAgent { agent }).await {
            Ok(DaemonResponse::Success { .. }) => {
                self.set_status(format!("✓ agent '{}' created", name.trim()).as_str());
                self.refresh().await;
            }
            Ok(DaemonResponse::Error { message }) => self.set_status(&format!("✗ {}", message)),
            Err(e) => self.set_status(&format!("✗ {}", e)),
            _ => {}
        }
        self.agent_mode = AgentMode::List;
    }

    async fn update_agent_model(&mut self, agent_id: String, model: String) {
        let agent = serde_json::json!({ "model": model.trim(), "provider": infer_provider(&model) });
        match daemon_request(&self.config, DaemonRequest::UpdateAgent { id: agent_id, agent }).await {
            Ok(DaemonResponse::Success { .. }) => {
                self.set_status(&format!("✓ model updated to {}", model.trim()));
                self.refresh().await;
            }
            Ok(DaemonResponse::Error { message }) => self.set_status(&format!("✗ {}", message)),
            Err(e) => self.set_status(&format!("✗ {}", e)),
            _ => {}
        }
        self.agent_mode = AgentMode::List;
    }

    async fn delete_agent_by_id(&mut self, agent_id: String, agent_name: String) {
        match daemon_request(&self.config, DaemonRequest::DeleteAgent { id: agent_id }).await {
            Ok(DaemonResponse::Success { .. }) => {
                self.set_status(&format!("✓ '{}' deleted", agent_name));
                self.selected_agent = self.selected_agent.saturating_sub(1);
                self.refresh().await;
            }
            Ok(DaemonResponse::Error { message }) => self.set_status(&format!("✗ {}", message)),
            Err(e) => self.set_status(&format!("✗ {}", e)),
            _ => {}
        }
        self.agent_mode = AgentMode::List;
    }

    async fn send_message(&mut self) {
        let input = self.composer_input.trim().to_string();
        if input.is_empty() { return; }
        if !self.daemon_ok { self.set_status("✗ daemon not running"); return; }
        if self.pending.is_some() { self.set_status("✗ waiting for response..."); return; }

        let agent_name = self.active_name();
        if agent_name == "—" { self.set_status("✗ no agent selected"); return; }

        let run_num = self.next_run_num();
        match daemon_request(
            &self.config,
            DaemonRequest::RunAgent { id: agent_name.clone(), input: Some(input.clone()) },
        ).await {
            Ok(DaemonResponse::ExecutionStarted { execution_id }) => {
                let msg = ChatMsg {
                    run_num,
                    input,
                    response: None,
                    status: ExecStatus::Running,
                    started_at: chrono::Utc::now().to_rfc3339(),
                    typewriter_pos: 0,
                };
                let msgs = self.chat.entry(agent_name.clone()).or_default();
                let msg_idx = msgs.len();
                msgs.push(msg);
                self.pending = Some(Pending { exec_id: execution_id, agent_name, msg_idx });
                self.composer_input.clear();
                self.chat_scroll = u16::MAX;
                self.chat_mode = ChatMode::Viewing;
            }
            Ok(DaemonResponse::Error { message }) => self.set_status(&format!("✗ {}", message)),
            Err(e) => self.set_status(&format!("✗ {}", e)),
            _ => {}
        }
    }

    async fn start_tool_generation(&mut self, name: String, purpose: String) {
        if !self.daemon_ok { self.set_status("✗ daemon not running"); return; }
        let engineer_id = Self::tool_engineer_id().to_string();

        let input = format!(
            "Create a tool named '{}'. Purpose: {}",
            name.trim(),
            purpose.trim()
        );
        match daemon_request(
            &self.config,
            DaemonRequest::RunAgent { id: engineer_id, input: Some(input) },
        ).await {
            Ok(DaemonResponse::ExecutionStarted { execution_id }) => {
                self.tool_mode = ToolMode::Generating { name: name.clone() };
                self.tool_pending = Some(ToolPending { exec_id: execution_id, name });
            }
            Ok(DaemonResponse::Error { message }) => {
                self.set_status(&format!("✗ MIND: {}", message));
                self.tool_mode = ToolMode::List;
            }
            Err(e) => {
                self.set_status(&format!("✗ {}", e));
                self.tool_mode = ToolMode::List;
            }
            _ => {}
        }
    }

    async fn delete_tool(&mut self, tool_id: String) {
        match daemon_request(&self.config, DaemonRequest::DeleteTool { id: tool_id }).await {
            Ok(DaemonResponse::Success { .. }) => {
                self.set_status("✓ tool deleted");
                self.tools = merge_tools(load_installed_tools(), match daemon_request(&self.config, DaemonRequest::ListTools).await { Ok(DaemonResponse::ToolList { tools }) => tools, _ => vec![] });
                if !self.tools.is_empty() {
                    self.selected_tool = self.selected_tool.min(self.tools.len() - 1);
                }
            }
            Ok(DaemonResponse::Error { message }) => self.set_status(&format!("✗ {}", message)),
            Err(e) => self.set_status(&format!("✗ {}", e)),
            _ => {}
        }
        self.tool_mode = ToolMode::List;
    }

    async fn do_pull_tool(&mut self, name: String) {
        match registry_pull(&name, &self.config.registry_owner, &self.config.registry_repo).await {
            Ok(manifest) => {
                // Register as a ToolResource in the DB so the panel shows it
                let tool_name = manifest.get("name").and_then(|v| v.as_str()).unwrap_or(&name).to_string();
                let description = manifest.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let parameters = manifest.get("parameters").cloned()
                    .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}}));
                let install_dir = dirs::home_dir().unwrap_or_default().join(".agenta/tools").join(&name);
                let handler_file = manifest.get("handler").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let handler = format!("/usr/bin/env bash {}", install_dir.join(&handler_file).display());

                let tool_val = serde_json::json!({
                    "name": tool_name,
                    "description": description,
                    "parameters": parameters,
                    "handler": handler,
                });
                let _ = daemon_request(&self.config, DaemonRequest::CreateTool { tool: tool_val }).await;

                self.tools = merge_tools(load_installed_tools(), match daemon_request(&self.config, DaemonRequest::ListTools).await { Ok(DaemonResponse::ToolList { tools }) => tools, _ => vec![] });
                self.set_status(&format!("✓ '{}' installed", name));
                self.tool_mode = ToolMode::PullAttach { name, input: String::new() };
            }
            Err(e) => {
                self.set_status(&format!("✗ pull failed: {}", e));
                self.tool_mode = ToolMode::List;
            }
        }
    }

    async fn do_attach_tool(&mut self, agent_id: String, agent_name: String, tool_name: String) {
        let tool = match read_installed_tool(&tool_name) {
            Ok(t) => t,
            Err(e) => {
                self.set_status(&format!("✗ {}", e));
                self.agent_mode = AgentMode::List;
                return;
            }
        };

        let mut agent: Agent = match daemon_request(
            &self.config, DaemonRequest::GetAgent { id: agent_id.clone() }
        ).await {
            Ok(DaemonResponse::AgentDetails { agent }) => match serde_json::from_value(agent) {
                Ok(a) => a,
                Err(e) => { self.set_status(&format!("✗ {}", e)); self.agent_mode = AgentMode::List; return; }
            },
            Ok(DaemonResponse::Error { message }) => {
                self.set_status(&format!("✗ {}", message)); self.agent_mode = AgentMode::List; return;
            }
            Err(e) => { self.set_status(&format!("✗ {}", e)); self.agent_mode = AgentMode::List; return; }
            _ => { self.agent_mode = AgentMode::List; return; }
        };

        let action = if let Some(pos) = agent.tools.iter().position(|t| t.name == tool.name) {
            agent.tools[pos] = tool;
            "updated"
        } else {
            agent.tools.push(tool);
            "attached"
        };
        if let Some(cfg) = agent.deep_agent_config.as_mut() {
            cfg.available_tools = agent.tools.iter().map(|t| t.name.clone()).collect();
        }
        agent.touch();

        match daemon_request(&self.config, DaemonRequest::UpdateAgent {
            id: agent.id.clone(),
            agent: serde_json::to_value(agent).unwrap_or_default(),
        }).await {
            Ok(DaemonResponse::Success { .. }) => {
                self.set_status(&format!("✓ '{}' {} on {}", tool_name, action, agent_name));
                self.refresh().await;
            }
            Ok(DaemonResponse::Error { message }) => self.set_status(&format!("✗ {}", message)),
            Err(e) => self.set_status(&format!("✗ {}", e)),
            _ => {}
        }
        self.agent_mode = AgentMode::List;
    }

    async fn do_attach_tool_by_name(&mut self, tool_name: String, agent_name: String) {
        // Resolve agent by name first
        let agents = self.agents.clone();
        let agent_val = agents.iter().find(|a| {
            a["name"].as_str().map(|n| n.eq_ignore_ascii_case(&agent_name)).unwrap_or(false)
        });
        let agent_id = match agent_val.and_then(|a| a["id"].as_str()) {
            Some(id) => id.to_string(),
            None => {
                self.set_status(&format!("✗ agent '{}' not found", agent_name));
                self.tool_mode = ToolMode::List;
                return;
            }
        };
        let aname = agent_name.clone();
        self.do_attach_tool(agent_id, aname, tool_name).await;
        self.tool_mode = ToolMode::List;
    }
}

// ── Installed tools (disk scan) ───────────────────────────────────────────────

fn load_installed_tools() -> Vec<serde_json::Value> {
    let tools_dir = match dirs::home_dir() {
        Some(h) => h.join(".agenta/tools"),
        None => return vec![],
    };
    let Ok(entries) = std::fs::read_dir(&tools_dir) else { return vec![]; };

    let mut tools = vec![];
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() { continue; }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() { continue; }
        let Ok(content) = std::fs::read_to_string(&manifest_path) else { continue; };
        let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&content) else { continue; };

        // Resolve handler to absolute path if it's just a filename
        if let Some(handler) = val.get("handler").and_then(|v| v.as_str()) {
            if !handler.starts_with('/') && !handler.starts_with("/usr") {
                let abs = format!("/usr/bin/env bash {}", path.join(handler).display());
                val["handler"] = serde_json::Value::String(abs);
            }
        }
        // Ensure fields the renderer expects
        if val.get("enabled").is_none() {
            val["enabled"] = serde_json::Value::Bool(true);
        }
        tools.push(val);
    }
    tools.sort_by(|a, b| {
        a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
    });
    tools
}

fn merge_tools(
    disk: Vec<serde_json::Value>,
    db: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for t in disk.iter().chain(db.iter()) {
        let name = t["name"].as_str().unwrap_or("").to_string();
        if seen.insert(name) {
            out.push(t.clone());
        }
    }
    out.sort_by(|a, b| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")));
    out
}

// ── Registry pull ─────────────────────────────────────────────────────────────

async fn registry_pull(name: &str, owner: &str, repo: &str) -> anyhow::Result<serde_json::Value> {
    let base = format!(
        "https://raw.githubusercontent.com/{}/{}/main/{}",
        owner, repo, name
    );
    let client = reqwest::Client::new();

    let resp = client.get(format!("{}/manifest.json", base)).send().await
        .map_err(|e| anyhow::anyhow!("Registry unreachable: {}", e))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow::anyhow!("'{}' not found in registry", name));
    }
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("Registry returned {}", resp.status()));
    }
    let manifest: serde_json::Value = resp.json().await
        .map_err(|_| anyhow::anyhow!("Invalid manifest.json"))?;

    let handler_file = manifest.get("handler").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("manifest.json missing 'handler'"))?.to_string();

    let script_bytes = client.get(format!("{}/{}", base, handler_file)).send().await?.bytes().await?;

    let install_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".agenta/tools").join(name);
    tokio::fs::create_dir_all(&install_dir).await?;
    tokio::fs::write(install_dir.join("manifest.json"), serde_json::to_string_pretty(&manifest)?).await?;
    let handler_path = install_dir.join(&handler_file);
    tokio::fs::write(&handler_path, &script_bytes).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&handler_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&handler_path, perms)?;
    }
    Ok(manifest)
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
        if app.tool_pending.is_some() {
            app.poll_tool_pending().await;
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

async fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    if code == KeyCode::Char('c') && mods == KeyModifiers::CONTROL {
        return true;
    }

    // Chat compose mode — capture all input
    if app.chat_mode == ChatMode::Composing {
        match code {
            KeyCode::Esc => app.chat_mode = ChatMode::Viewing,
            KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                app.composer_input.push(c);
            }
            KeyCode::Backspace => { app.composer_input.pop(); }
            KeyCode::Enter => app.send_message().await,
            _ => {}
        }
        return false;
    }

    // Agent wizard / edit / delete / view modes — capture all input
    let amode = app.agent_mode.clone();
    match amode {
        AgentMode::View { .. } => {
            match code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('v') => {
                    app.agent_mode = AgentMode::List;
                }
                _ => {}
            }
            return false;
        }
        AgentMode::WizardName(cur) => {
            match code {
                KeyCode::Esc => app.agent_mode = AgentMode::List,
                KeyCode::Enter => {
                    if !cur.trim().is_empty() {
                        app.agent_mode = AgentMode::WizardModel { name: cur.trim().to_string(), model: String::new() };
                    }
                }
                KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                    let mut s = cur; s.push(c);
                    app.agent_mode = AgentMode::WizardName(s);
                }
                KeyCode::Backspace => { let mut s = cur; s.pop(); app.agent_mode = AgentMode::WizardName(s); }
                _ => {}
            }
            return false;
        }
        AgentMode::WizardModel { name, model } => {
            match code {
                KeyCode::Esc => app.agent_mode = AgentMode::WizardName(name),
                KeyCode::Enter => {
                    if !model.trim().is_empty() {
                        let n = name.clone(); let m = model.clone();
                        app.create_agent(n, m).await;
                    }
                }
                KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                    let mut m = model; m.push(c);
                    app.agent_mode = AgentMode::WizardModel { name, model: m };
                }
                KeyCode::Backspace => { let mut m = model; m.pop(); app.agent_mode = AgentMode::WizardModel { name, model: m }; }
                _ => {}
            }
            return false;
        }
        AgentMode::EditModel { agent_id, agent_name, model } => {
            match code {
                KeyCode::Esc => app.agent_mode = AgentMode::List,
                KeyCode::Enter => {
                    if !model.trim().is_empty() {
                        let id = agent_id.clone(); let m = model.clone();
                        app.update_agent_model(id, m).await;
                    }
                }
                KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                    let mut m = model; m.push(c);
                    app.agent_mode = AgentMode::EditModel { agent_id, agent_name, model: m };
                }
                KeyCode::Backspace => {
                    let mut m = model; m.pop();
                    app.agent_mode = AgentMode::EditModel { agent_id, agent_name, model: m };
                }
                _ => {}
            }
            return false;
        }
        AgentMode::ConfirmDelete { agent_id, agent_name } => {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    app.delete_agent_by_id(agent_id, agent_name).await;
                }
                _ => app.agent_mode = AgentMode::List,
            }
            return false;
        }
        AgentMode::AttachTool { agent_id, agent_name, input } => {
            match code {
                KeyCode::Esc => app.agent_mode = AgentMode::List,
                KeyCode::Enter => {
                    let tool = input.trim().to_string();
                    if !tool.is_empty() {
                        let id = agent_id.clone();
                        let aname = agent_name.clone();
                        app.do_attach_tool(id, aname, tool).await;
                    }
                }
                KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                    let mut s = input; s.push(c);
                    app.agent_mode = AgentMode::AttachTool { agent_id, agent_name, input: s };
                }
                KeyCode::Backspace => {
                    let mut s = input; s.pop();
                    app.agent_mode = AgentMode::AttachTool { agent_id, agent_name, input: s };
                }
                _ => {}
            }
            return false;
        }
        AgentMode::List => {}
    }

    // Tool wizard text input modes — capture all input
    let mode = app.tool_mode.clone();
    match mode {
        ToolMode::WizardName(current) => {
            match code {
                KeyCode::Esc => app.tool_mode = ToolMode::List,
                KeyCode::Enter => {
                    if !current.trim().is_empty() {
                        app.tool_mode = ToolMode::WizardPurpose {
                            name: current.trim().to_string(),
                            purpose: String::new(),
                        };
                    }
                }
                KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                    let mut s = current;
                    s.push(c);
                    app.tool_mode = ToolMode::WizardName(s);
                }
                KeyCode::Backspace => {
                    let mut s = current;
                    s.pop();
                    app.tool_mode = ToolMode::WizardName(s);
                }
                _ => {}
            }
            return false;
        }
        ToolMode::WizardPurpose { name, purpose } => {
            match code {
                KeyCode::Esc => app.tool_mode = ToolMode::WizardName(name),
                KeyCode::Enter => {
                    if !purpose.trim().is_empty() {
                        let n = name.clone();
                        let p = purpose.clone();
                        app.start_tool_generation(n, p).await;
                    }
                }
                KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                    let mut p = purpose;
                    p.push(c);
                    app.tool_mode = ToolMode::WizardPurpose { name, purpose: p };
                }
                KeyCode::Backspace => {
                    let mut p = purpose;
                    p.pop();
                    app.tool_mode = ToolMode::WizardPurpose { name, purpose: p };
                }
                _ => {}
            }
            return false;
        }
        ToolMode::Generating { .. } => {
            // Waiting for MIND — ignore all keys
            return false;
        }
        ToolMode::Done { .. } => {
            app.tool_mode = ToolMode::List;
            return false;
        }
        ToolMode::ConfirmDelete { tool_id, .. } => {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    app.delete_tool(tool_id).await;
                }
                _ => app.tool_mode = ToolMode::List,
            }
            return false;
        }
        ToolMode::PullName(current) => {
            match code {
                KeyCode::Esc => app.tool_mode = ToolMode::List,
                KeyCode::Enter => {
                    let name = current.trim().to_string();
                    if !name.is_empty() {
                        app.do_pull_tool(name).await;
                    }
                }
                KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                    let mut s = current; s.push(c);
                    app.tool_mode = ToolMode::PullName(s);
                }
                KeyCode::Backspace => { let mut s = current; s.pop(); app.tool_mode = ToolMode::PullName(s); }
                _ => {}
            }
            return false;
        }
        ToolMode::PullAttach { name, input } => {
            match code {
                KeyCode::Esc => app.tool_mode = ToolMode::List,
                KeyCode::Enter => {
                    let agent = input.trim().to_string();
                    if agent.is_empty() {
                        app.tool_mode = ToolMode::List;
                    } else {
                        let n = name.clone();
                        app.do_attach_tool_by_name(n, agent).await;
                    }
                }
                KeyCode::Char(c) if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT => {
                    let mut s = input; s.push(c);
                    app.tool_mode = ToolMode::PullAttach { name, input: s };
                }
                KeyCode::Backspace => { let mut s = input; s.pop(); app.tool_mode = ToolMode::PullAttach { name, input: s }; }
                _ => {}
            }
            return false;
        }
        ToolMode::List => {}
    }

    // Global nav
    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Tab | KeyCode::Right => { app.active_panel = app.active_panel.next(); }
        KeyCode::BackTab | KeyCode::Left => { app.active_panel = app.active_panel.prev(); }
        _ => {}
    }

    // Panel-specific keys
    match app.active_panel {
        Panel::Agents => match code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !app.agents.is_empty() {
                    app.selected_agent = (app.selected_agent + 1).min(app.agents.len() - 1);
                    app.chat_scroll = u16::MAX;
                    app.exec_scroll = 0;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.selected_agent = app.selected_agent.saturating_sub(1);
                app.chat_scroll = u16::MAX;
                app.exec_scroll = 0;
            }
            KeyCode::Enter => app.active_panel = Panel::Chat,
            KeyCode::Char('n') => {
                app.agent_mode = AgentMode::WizardName(String::new());
            }
            KeyCode::Char('a') => {
                if let Some(agent) = app.agents.get(app.selected_agent) {
                    let agent_id = agent["id"].as_str().unwrap_or("").to_string();
                    let agent_name = agent["name"].as_str().unwrap_or("?").to_string();
                    if !agent_id.is_empty() {
                        app.agent_mode = AgentMode::AttachTool { agent_id, agent_name, input: String::new() };
                    }
                }
            }
            KeyCode::Char('e') => {
                if let Some(agent) = app.agents.get(app.selected_agent) {
                    let agent_id = agent["id"].as_str().unwrap_or("").to_string();
                    let agent_name = agent["name"].as_str().unwrap_or("?").to_string();
                    let model = agent["model"].as_str().unwrap_or("").to_string();
                    if !agent_id.is_empty() {
                        app.agent_mode = AgentMode::EditModel { agent_id, agent_name, model };
                    }
                }
            }
            KeyCode::Char('v') => {
                if let Some(agent) = app.agents.get(app.selected_agent) {
                    app.agent_mode = AgentMode::View { agent: agent.clone() };
                }
            }
            KeyCode::Char('d') => {
                if let Some(agent) = app.agents.get(app.selected_agent) {
                    let agent_id = agent["id"].as_str().unwrap_or("").to_string();
                    let agent_name = agent["name"].as_str().unwrap_or("?").to_string();
                    if !agent_id.is_empty() {
                        app.agent_mode = AgentMode::ConfirmDelete { agent_id, agent_name };
                    }
                }
            }
            _ => {}
        }
        Panel::Executions => match code {
            KeyCode::Down | KeyCode::Char('j') => {
                app.exec_scroll = app.exec_scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.exec_scroll = app.exec_scroll.saturating_sub(1);
            }
            _ => {}
        }
        Panel::Chat => match code {
            KeyCode::Down | KeyCode::Char('j') => {
                app.chat_scroll = app.chat_scroll.saturating_add(1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.chat_scroll = app.chat_scroll.saturating_sub(1);
            }
            KeyCode::Char('g') => app.chat_scroll = 0,
            KeyCode::Char('G') => app.chat_scroll = u16::MAX,
            KeyCode::Char('i') | KeyCode::Enter => {
                app.chat_mode = ChatMode::Composing;
                app.active_panel = Panel::Chat;
            }
            _ => {}
        }
        Panel::Tools => match code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !app.tools.is_empty() {
                    app.selected_tool = (app.selected_tool + 1).min(app.tools.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.selected_tool = app.selected_tool.saturating_sub(1);
            }
            KeyCode::Char('n') => {
                app.tool_mode = ToolMode::WizardName(String::new());
            }
            KeyCode::Char('p') => {
                app.tool_mode = ToolMode::PullName(String::new());
            }
            KeyCode::Char('d') => {
                if let Some(tool) = app.tools.get(app.selected_tool) {
                    let tool_id = tool["id"].as_str().unwrap_or("").to_string();
                    let tool_name = tool["name"].as_str().unwrap_or("?").to_string();
                    if !tool_id.is_empty() {
                        app.tool_mode = ToolMode::ConfirmDelete { tool_id, tool_name };
                    }
                }
            }
            _ => {}
        }
    }

    false
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let [topbar, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
    ]).areas(area);

    let [top_half, bottom_half] = Layout::vertical([
        Constraint::Percentage(38),
        Constraint::Percentage(62),
    ]).areas(body);

    let [agents_area, executions_area] = Layout::horizontal([
        Constraint::Percentage(32),
        Constraint::Percentage(68),
    ]).areas(top_half);

    let [chat_area, tools_area] = Layout::horizontal([
        Constraint::Percentage(62),
        Constraint::Percentage(38),
    ]).areas(bottom_half);

    render_topbar(f, app, topbar);
    render_agents(f, app, agents_area);
    render_executions(f, app, executions_area);
    render_chat(f, app, chat_area);
    render_tools(f, app, tools_area);
}

// ── Topbar ─────────────────────────────────────────────────────────────────────

fn render_topbar(f: &mut Frame, app: &App, area: Rect) {
    let ver = if app.daemon_version.is_empty() { "—".to_string() } else { format!("v{}", app.daemon_version) };
    let (dot, dcol) = if app.daemon_ok { ("●", Color::Green) } else { ("✗", Color::Red) };

    let status_part = if let Some((msg, _)) = &app.status_msg {
        let col = if msg.starts_with('✗') { Color::Red } else { Color::Green };
        Span::styled(format!("  {}", msg), Style::default().fg(col))
    } else {
        Span::styled(
            "  Tab:panel  j/k:nav  i:compose  n:new tool  d:delete  q:quit",
            Style::default().fg(Color::from_u32(0x3a3a3a)),
        )
    };

    let mut spans = vec![
        Span::styled("agenta ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(ver, Style::default().fg(Color::DarkGray)),
        Span::styled("  daemon ", Style::default().fg(Color::from_u32(0x3a3a3a))),
        Span::styled(dot, Style::default().fg(dcol)),
        status_part,
    ];

    // Spinner if anything is pending
    if app.pending.is_some() || app.tool_pending.is_some() {
        let spin = SPINNER[app.spinner_tick % SPINNER.len()];
        spans.push(Span::styled(format!("  {}", spin), Style::default().fg(Color::Yellow)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── Agents panel (top-left) ────────────────────────────────────────────────────

fn render_agents(f: &mut Frame, app: &App, area: Rect) {
    let active = app.active_panel == Panel::Agents || !matches!(app.agent_mode, AgentMode::List);
    let block = panel_block("agents", active);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Overlay modes take over the panel
    match &app.agent_mode.clone() {
        AgentMode::View { agent } => {
            return render_agent_view(f, agent, inner);
        }
        AgentMode::WizardName(name) => {
            return render_agent_wizard_name(f, name, inner);
        }
        AgentMode::WizardModel { name, model } => {
            return render_agent_wizard_model(f, name, model, inner);
        }
        AgentMode::EditModel { agent_name, model, .. } => {
            return render_agent_edit_model(f, agent_name, model, inner);
        }
        AgentMode::ConfirmDelete { agent_name, .. } => {
            return render_agent_confirm_delete(f, agent_name, inner);
        }
        AgentMode::AttachTool { agent_name, input, .. } => {
            return render_agent_attach_tool(f, agent_name, input, inner);
        }
        AgentMode::List => {}
    }

    // Footer hints (2 lines)
    let footer_h = 2u16;
    let [list_area, footer_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(footer_h),
    ]).areas(inner);

    if app.agents.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("  no agents", Style::default().fg(Color::DarkGray))),
                Line::from(""),
                Line::from(Span::styled("  [n] create new", Style::default().fg(Color::DarkGray))),
            ]),
            list_area,
        );
    } else {
        let mut lines: Vec<Line> = vec![];
        for (i, agent) in app.agents.iter().enumerate() {
            let name = agent["name"].as_str().unwrap_or("?");
            let short = abbrev(name);
            let model = agent["model"].as_str().unwrap_or("?");
            let model_short: String = model.chars().take(11).collect();
            let status = agent["status"].as_str().unwrap_or("idle");
            let deep = !agent["deep_agent_config"].is_null();

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
            let deep_tag = if deep { Span::styled("⚡", Style::default().fg(Color::Yellow)) } else { Span::raw(" ") };

            lines.push(Line::from(vec![
                Span::raw(prefix),
                Span::styled(dot, Style::default().fg(dot_color)),
                Span::raw(" "),
                Span::styled(format!("{:<7}", short.chars().take(7).collect::<String>()), name_style),
                Span::raw(" "),
                deep_tag,
                Span::styled(
                    format!(" {}", model_short),
                    Style::default().fg(Color::from_u32(0x444444)),
                ),
            ]));
        }

        let h = list_area.height as usize;
        let scroll = if app.selected_agent >= h {
            (app.selected_agent + 1).saturating_sub(h) as u16
        } else { 0 };

        f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), list_area);
    }

    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" [n]", Style::default().fg(Color::Cyan)),
                Span::styled(" new  ", Style::default().fg(Color::DarkGray)),
                Span::styled("[v]", Style::default().fg(Color::Cyan)),
                Span::styled(" view  ", Style::default().fg(Color::DarkGray)),
                Span::styled("[e]", Style::default().fg(Color::Cyan)),
                Span::styled(" edit  ", Style::default().fg(Color::DarkGray)),
                Span::styled("[d]", Style::default().fg(Color::Cyan)),
                Span::styled(" delete", Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(vec![
                Span::styled(" [a]", Style::default().fg(Color::Cyan)),
                Span::styled(" attach tool", Style::default().fg(Color::DarkGray)),
            ]),
        ]),
        footer_area,
    );
}

fn render_agent_view(f: &mut Frame, agent: &serde_json::Value, area: Rect) {
    let label = |s: &str| Span::styled(format!("  {:<12}", s), Style::default().fg(Color::DarkGray));
    let value = |s: String| Span::styled(s, Style::default().fg(Color::White));
    let cyan  = |s: String| Span::styled(s, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

    let name     = agent["name"].as_str().unwrap_or("?").to_string();
    let model    = agent["model"].as_str().unwrap_or("?").to_string();
    let provider = agent["provider"].as_str().unwrap_or("—").to_string();
    let status   = agent["status"].as_str().unwrap_or("?").to_string();
    let mode     = agent["execution_mode"].as_str().unwrap_or("?").to_string();
    let memory   = if agent["memory_enabled"].as_bool().unwrap_or(false) { "on" } else { "off" };
    let deep     = if !agent["deep_agent_config"].is_null() { "on" } else { "off" };
    let schedule = agent["schedule"].as_str().unwrap_or("—").to_string();
    let runs     = agent["run_count"].as_u64().unwrap_or(0).to_string();

    // Tools list
    let tools_str = agent["tools"]
        .as_array()
        .map(|arr| {
            let names: Vec<&str> = arr.iter()
                .filter_map(|t| t["name"].as_str())
                .collect();
            if names.is_empty() { "—".to_string() } else { names.join(", ") }
        })
        .unwrap_or_else(|| "—".to_string());

    // System prompt — show first ~120 chars
    let prompt_raw = agent["system_prompt"].as_str().unwrap_or("").trim().to_string();
    let prompt_preview: String = prompt_raw.chars().take(200).collect();
    let prompt_display = if prompt_raw.len() > 200 {
        format!("{}…", prompt_preview)
    } else {
        prompt_preview
    };

    let mut lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(cyan(format!("  {}", name))),
        Line::from(""),
        Line::from(vec![label("Model"),    value(model)]),
        Line::from(vec![label("Provider"), value(provider)]),
        Line::from(vec![label("Status"),   value(status)]),
        Line::from(vec![label("Mode"),     value(mode)]),
        Line::from(vec![label("Schedule"), value(schedule)]),
        Line::from(vec![label("Memory"),   value(memory.to_string())]),
        Line::from(vec![label("Deep"),     value(deep.to_string())]),
        Line::from(vec![label("Runs"),     value(runs)]),
        Line::from(vec![label("Tools"),    value(tools_str)]),
        Line::from(""),
        Line::from(Span::styled("  Prompt:", Style::default().fg(Color::DarkGray))),
    ];

    // Word-wrap prompt into ~(area.width - 4) chars per line
    let wrap_width = (area.width as usize).saturating_sub(4).max(20);
    for chunk in prompt_display.chars().collect::<Vec<_>>().chunks(wrap_width) {
        let s: String = chunk.iter().collect();
        lines.push(Line::from(Span::styled(format!("  {}", s), Style::default().fg(Color::from_u32(0x888888)))));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  [Esc] back",
        Style::default().fg(Color::DarkGray),
    )));

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_agent_wizard_name(f: &mut Frame, name: &str, area: Rect) {
    let cursor = if name.is_empty() { "▌" } else { "" };
    f.render_widget(Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(" New agent  1/2", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(Span::styled(" Agent name:", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            Span::styled(name, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" [Enter] next  [Esc] cancel", Style::default().fg(Color::from_u32(0x3a3a3a)))),
    ]), area);
}

fn render_agent_wizard_model(f: &mut Frame, name: &str, model: &str, area: Rect) {
    let cursor = "▌";
    let provider = infer_provider(model);
    f.render_widget(Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(" New agent  2/2", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
        Line::from(vec![
            Span::styled(" name: ", Style::default().fg(Color::DarkGray)),
            Span::styled(name, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" Model:", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            Span::styled(model, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("   provider: ", Style::default().fg(Color::from_u32(0x444444))),
            Span::styled(provider, Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" [Enter] create  [Esc] back", Style::default().fg(Color::from_u32(0x3a3a3a)))),
    ]), area);
}

fn render_agent_edit_model(f: &mut Frame, agent_name: &str, model: &str, area: Rect) {
    let provider = infer_provider(model);
    f.render_widget(Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(" Edit model", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
        Line::from(vec![
            Span::styled(" agent: ", Style::default().fg(Color::DarkGray)),
            Span::styled(abbrev(agent_name), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" Model:", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            Span::styled(model, Style::default().fg(Color::White)),
            Span::styled("▌", Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("   provider: ", Style::default().fg(Color::from_u32(0x444444))),
            Span::styled(provider, Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" [Enter] save  [Esc] cancel", Style::default().fg(Color::from_u32(0x3a3a3a)))),
    ]), area);
}

fn render_agent_confirm_delete(f: &mut Frame, agent_name: &str, area: Rect) {
    f.render_widget(Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(" Delete agent?", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(vec![
            Span::styled(" name: ", Style::default().fg(Color::DarkGray)),
            Span::styled(agent_name, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(" [y]", Style::default().fg(Color::Red)),
            Span::styled(" yes  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[Esc]", Style::default().fg(Color::Cyan)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]),
    ]), area);
}

// ── Executions panel (top-right) ──────────────────────────────────────────────

fn render_executions(f: &mut Frame, app: &App, area: Rect) {
    let short = app.active_short();
    let active = app.active_panel == Panel::Executions;
    let block = panel_block(format!("runs · {}", short), active);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let runs = app.runs_for_active();
    if runs.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("  no runs yet", Style::default().fg(Color::DarkGray))),
            inner,
        );
        return;
    }

    let w = inner.width as usize;
    let mut lines: Vec<Line> = vec![];

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

fn render_chat(f: &mut Frame, app: &mut App, area: Rect) {
    let composing = app.chat_mode == ChatMode::Composing;
    let active = app.active_panel == Panel::Chat || composing;
    let short = app.active_short();
    let block = panel_block(format!("chat · {}", short), active);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Split: messages (top ~75%) + composer (bottom ~25%, min 4 lines)
    let composer_h = (inner.height / 4).max(4).min(inner.height.saturating_sub(4));

    let [msgs_area, composer_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(composer_h),
    ]).areas(inner);

    render_chat_messages(f, app, msgs_area);
    render_chat_composer(f, app, composer_area);
}

fn render_chat_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let msgs = app.active_chat();
    let short = app.active_short();

    if msgs.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  no conversation yet",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "  press i or Enter to start chatting",
                    Style::default().fg(Color::from_u32(0x3a3a3a)),
                )),
            ]),
            area,
        );
        return;
    }

    let mut lines: Vec<Line> = vec![];

    for (msg_i, msg) in msgs.iter().enumerate() {
        let ts = fmt_ts(&msg.started_at);
        lines.push(Line::from(Span::styled(
            format!(" ── #{} · {} ──", msg.run_num, ts),
            Style::default().fg(Color::from_u32(0x333333)),
        )));
        lines.push(Line::from(""));

        lines.push(Line::from(vec![
            Span::styled(" you", Style::default().fg(Color::DarkGray)),
            Span::styled(" › ", Style::default().fg(Color::DarkGray)),
            Span::styled(msg.input.clone(), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(""));

        match &msg.status {
            ExecStatus::Running => {
                let spin = SPINNER[app.spinner_tick % SPINNER.len()];
                let stream_slice = {
                    let len = app.stream_lines.len();
                    &app.stream_lines[len.saturating_sub(3)..]
                };
                for sline in stream_slice {
                    lines.push(Line::from(Span::styled(
                        format!("   {}", sline),
                        Style::default().fg(Color::from_u32(0x3a3a3a)),
                    )));
                }
                lines.push(Line::from(vec![
                    Span::styled(format!(" {}", short), Style::default().fg(Color::Cyan)),
                    Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{} thinking...", spin),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            ExecStatus::Completed => {
                if let Some(resp) = &msg.response {
                    let is_latest = msg_i == msgs.len() - 1;
                    let char_total = resp.chars().count();
                    let is_animating = is_latest && msg.typewriter_pos < char_total;
                    let display: String = if is_animating {
                        resp.chars().take(msg.typewriter_pos).collect()
                    } else {
                        resp.clone()
                    };

                    for (i, part) in display.lines().enumerate() {
                        let line_count = display.lines().count();
                        let is_last = i + 1 == line_count;
                        if i == 0 {
                            let mut spans = vec![
                                Span::styled(format!(" {}", short), Style::default().fg(Color::Cyan)),
                                Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                                Span::styled(part.to_string(), Style::default().fg(Color::White)),
                            ];
                            if is_animating && is_last {
                                spans.push(Span::styled("▌", Style::default().fg(Color::Cyan)));
                            }
                            lines.push(Line::from(spans));
                        } else {
                            let indent = " ".repeat(short.len() + 5);
                            let mut spans = vec![Span::styled(
                                format!("{}{}", indent, part),
                                Style::default().fg(Color::White),
                            )];
                            if is_animating && is_last {
                                spans.push(Span::styled("▌", Style::default().fg(Color::Cyan)));
                            }
                            lines.push(Line::from(spans));
                        }
                    }
                }
            }
            ExecStatus::Failed(err) => {
                lines.push(Line::from(vec![
                    Span::styled(format!(" {}", short), Style::default().fg(Color::Cyan)),
                    Span::styled(" › ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("error: {}", err), Style::default().fg(Color::Red)),
                ]));
            }
        }
        lines.push(Line::from(""));
    }

    let total = lines.len() as u16;
    let max_scroll = total.saturating_sub(area.height);
    // Clamp and persist so scroll-up works immediately after a u16::MAX jump-to-bottom
    app.chat_scroll = app.chat_scroll.min(max_scroll);

    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((app.chat_scroll, 0)),
        area,
    );
}

fn render_chat_composer(f: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 { return; }

    // Divider
    f.render_widget(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::from_u32(0x333333))),
        area,
    );

    let inner = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    if inner.height == 0 { return; }

    let composing = app.chat_mode == ChatMode::Composing;
    let short = app.active_short();
    let model = app.active_agent().and_then(|a| a["model"].as_str()).unwrap_or("—");

    let [ctx_area, input_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
    ]).areas(inner);

    // Context line
    let ctx = if app.pending.is_some() {
        let spin = SPINNER[app.spinner_tick % SPINNER.len()];
        Line::from(vec![
            Span::styled(format!(" {} waiting for ", spin), Style::default().fg(Color::Yellow)),
            Span::styled(&short, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("...", Style::default().fg(Color::Yellow)),
        ])
    } else if composing {
        Line::from(vec![
            Span::styled(" → ", Style::default().fg(Color::Cyan)),
            Span::styled(&short, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" · {}", model), Style::default().fg(Color::DarkGray)),
            Span::styled("  [Esc to cancel]", Style::default().fg(Color::from_u32(0x3a3a3a))),
        ])
    } else {
        Line::from(vec![
            Span::styled(" talking to ", Style::default().fg(Color::DarkGray)),
            Span::styled(&short, Style::default().fg(Color::Cyan)),
            Span::styled(format!(" · {}  ", model), Style::default().fg(Color::DarkGray)),
            Span::styled("[i] compose", Style::default().fg(Color::from_u32(0x3a3a3a))),
        ])
    };
    f.render_widget(Paragraph::new(ctx), ctx_area);

    // Input line
    if inner.height >= 2 {
        let cursor = if composing { "▌" } else { "" };
        let input_style = if composing {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" › ", Style::default().fg(if composing { Color::Cyan } else { Color::DarkGray })),
                Span::styled(app.composer_input.clone(), input_style),
                Span::styled(cursor, Style::default().fg(Color::Cyan)),
            ])),
            input_area,
        );
    }
}

// ── Tools panel (bottom-right) ────────────────────────────────────────────────

fn render_tools(f: &mut Frame, app: &App, area: Rect) {
    let active = app.active_panel == Panel::Tools;
    let in_wizard = !matches!(app.tool_mode, ToolMode::List);
    let block = panel_block("tools", active || in_wizard);
    let inner = block.inner(area);
    f.render_widget(block, area);

    match &app.tool_mode.clone() {
        ToolMode::List => render_tools_list(f, app, inner),
        ToolMode::WizardName(name) => render_tools_wizard_name(f, name, inner),
        ToolMode::WizardPurpose { name, purpose } => render_tools_wizard_purpose(f, name, purpose, inner),
        ToolMode::Generating { name } => render_tools_generating(f, app, name, inner),
        ToolMode::Done { summary } => render_tools_done(f, summary, inner),
        ToolMode::ConfirmDelete { tool_name, .. } => render_tools_confirm_delete(f, tool_name, inner),
        ToolMode::PullName(name) => render_tools_pull_name(f, name, inner),
        ToolMode::PullAttach { name, input } => render_tools_pull_attach(f, name, input, inner),
    }
}

fn render_tools_list(f: &mut Frame, app: &App, area: Rect) {
    if app.tools.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("  no tools yet", Style::default().fg(Color::DarkGray))),
                Line::from(""),
                Line::from(Span::styled(
                    "  [n] create new with MIND",
                    Style::default().fg(Color::DarkGray),
                )),
            ]),
            area,
        );
        return;
    }

    let footer_h = 2u16;
    let [list_area, footer_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(footer_h),
    ]).areas(area);

    let mut lines: Vec<Line> = vec![];
    for (i, tool) in app.tools.iter().enumerate() {
        let name = tool["name"].as_str().unwrap_or("?");
        let handler = tool["handler"].as_str().unwrap_or("");
        let lang = infer_lang(handler);
        let enabled = tool["enabled"].as_bool().unwrap_or(true);

        let selected = i == app.selected_tool && app.active_panel == Panel::Tools;
        let (dot, dot_color) = if enabled { ("●", Color::Green) } else { ("○", Color::DarkGray) };

        let (prefix, name_style) = if selected {
            ("▶ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        } else {
            ("  ", Style::default().fg(Color::White))
        };

        let name_trunc: String = name.chars().take(18).collect();
        lines.push(Line::from(vec![
            Span::raw(prefix),
            Span::styled(dot, Style::default().fg(dot_color)),
            Span::raw(" "),
            Span::styled(format!("{:<18}", name_trunc), name_style),
            Span::styled(lang, Style::default().fg(Color::from_u32(0x444444))),
        ]));
    }

    let h = list_area.height as usize;
    let scroll = if app.selected_tool >= h {
        (app.selected_tool + 1).saturating_sub(h) as u16
    } else {
        0
    };
    let max_scroll = (lines.len() as u16).saturating_sub(list_area.height);

    f.render_widget(Paragraph::new(lines).scroll((scroll.min(max_scroll), 0)), list_area);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" [n]", Style::default().fg(Color::Cyan)),
                Span::styled(" new  ", Style::default().fg(Color::DarkGray)),
                Span::styled("[p]", Style::default().fg(Color::Cyan)),
                Span::styled(" pull  ", Style::default().fg(Color::DarkGray)),
                Span::styled("[d]", Style::default().fg(Color::Cyan)),
                Span::styled(" delete", Style::default().fg(Color::DarkGray)),
            ]),
        ]),
        footer_area,
    );
}

fn render_tools_wizard_name(f: &mut Frame, name: &str, area: Rect) {
    let cursor = if name.is_empty() { "▌" } else { "" };
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            " Create new tool  1/2",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(" Tool name:", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            Span::styled(name, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " [Enter] next  [Esc] cancel",
            Style::default().fg(Color::from_u32(0x3a3a3a)),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_tools_wizard_purpose(f: &mut Frame, name: &str, purpose: &str, area: Rect) {
    let cursor = "▌";
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            " Create new tool  2/2",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(" name: ", Style::default().fg(Color::DarkGray)),
            Span::styled(name, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" What should it do?", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            Span::styled(purpose, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " [Enter] generate  [Esc] back",
            Style::default().fg(Color::from_u32(0x3a3a3a)),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_tools_generating(f: &mut Frame, app: &App, name: &str, area: Rect) {
    let spin = SPINNER[app.spinner_tick % SPINNER.len()];
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(format!(" {} MIND is building ", spin), Style::default().fg(Color::Yellow)),
            Span::styled(name, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("...", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
    ];

    let stream_slice = {
        let len = app.tool_stream_lines.len();
        &app.tool_stream_lines[len.saturating_sub(6)..]
    };
    for sline in stream_slice {
        lines.push(Line::from(Span::styled(
            format!("  {}", sline),
            Style::default().fg(Color::from_u32(0x444444)),
        )));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_tools_done(f: &mut Frame, summary: &str, area: Rect) {
    let is_err = summary.starts_with('✗');
    let header_color = if is_err { Color::Red } else { Color::Green };
    let (icon, header) = if is_err { ("✗", " Failed") } else { ("✓", " Tool created") };

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!(" {} {}", icon, header),
            Style::default().fg(header_color).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for part in summary.lines() {
        lines.push(Line::from(Span::styled(
            format!(" {}", part),
            Style::default().fg(Color::White),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " [any key] back to list",
        Style::default().fg(Color::from_u32(0x3a3a3a)),
    )));

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_tools_confirm_delete(f: &mut Frame, tool_name: &str, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(" Delete tool?", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(vec![
            Span::styled(" name: ", Style::default().fg(Color::DarkGray)),
            Span::styled(tool_name, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(" [y]", Style::default().fg(Color::Red)),
            Span::styled(" yes  ", Style::default().fg(Color::DarkGray)),
            Span::styled("[Esc]", Style::default().fg(Color::Cyan)),
            Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_tools_pull_name(f: &mut Frame, name: &str, area: Rect) {
    let cursor = if name.is_empty() { "▌" } else { "" };
    f.render_widget(Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(" Pull from registry", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(Span::styled(" Tool name:", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            Span::styled(name, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " e.g. tavily_search, find_file",
            Style::default().fg(Color::from_u32(0x444444)),
        )),
        Line::from(""),
        Line::from(Span::styled(" [Enter] pull  [Esc] cancel", Style::default().fg(Color::from_u32(0x3a3a3a)))),
    ]), area);
}

fn render_tools_pull_attach(f: &mut Frame, tool_name: &str, agent_input: &str, area: Rect) {
    let cursor = if agent_input.is_empty() { "▌" } else { "" };
    f.render_widget(Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(" ✓ ", Style::default().fg(Color::Green)),
            Span::styled(tool_name, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(" installed", Style::default().fg(Color::Green)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" Attach to agent (optional):", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            Span::styled(agent_input, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" [Enter] attach  [Esc] skip", Style::default().fg(Color::from_u32(0x3a3a3a)))),
    ]), area);
}

fn render_agent_attach_tool(f: &mut Frame, agent_name: &str, input: &str, area: Rect) {
    let cursor = if input.is_empty() { "▌" } else { "" };
    f.render_widget(Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(" Attach tool", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
        Line::from(vec![
            Span::styled(" agent: ", Style::default().fg(Color::DarkGray)),
            Span::styled(abbrev(agent_name), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(Span::styled(" Installed tool name:", Style::default().fg(Color::DarkGray))),
        Line::from(vec![
            Span::styled(" › ", Style::default().fg(Color::Cyan)),
            Span::styled(input, Style::default().fg(Color::White)),
            Span::styled(cursor, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            " Tool must be installed first (Tools panel → [p])",
            Style::default().fg(Color::from_u32(0x444444)),
        )),
        Line::from(""),
        Line::from(Span::styled(" [Enter] attach  [Esc] cancel", Style::default().fg(Color::from_u32(0x3a3a3a)))),
    ]), area);
}
