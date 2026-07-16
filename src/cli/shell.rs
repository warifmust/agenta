use std::io::{stdout, Write};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    style::{Attribute, SetAttribute},
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use owo_colors::OwoColorize;

use crate::core::{AppConfig, DaemonRequest, DaemonResponse};
use super::commands::{daemon_request, styled_table};

/// All slash commands: (command, description). Single source of truth for the
/// live palette and `/help` so they can never drift apart.
const COMMANDS: &[(&str, &str)] = &[
    ("/create-agent", "Create a new agent (guided wizard)"),
    ("/update-agent", "Update an existing agent"),
    ("/create-tool",  "Create a new tool (guided wizard)"),
    ("/update-tool",  "Update an existing tool"),
    ("/list",         "List all agents"),
    ("/list-tools",   "List all tools"),
    ("/list-proposals", "List pending proposals"),
    ("/list-kb",      "List knowledge bases (RAG)"),
    ("/attach-kb",    "Attach a knowledge base to an agent"),
    ("/detach-kb",    "Detach a knowledge base from an agent"),
    ("/get",          "Show agent details"),
    ("/get-tool",     "Show tool details"),
    ("/run",          "Run an agent"),
    ("/stop",         "Stop a running agent"),
    ("/logs",         "View agent logs"),
    ("/delete",       "Delete an agent"),
    ("/status",       "Show daemon status"),
    ("/help",         "Show this help"),
    ("/quit",         "Exit the shell"),
];

const PROMPT: &str = "agenta> ";

/// Outcome of reading one line from the palette reader.
pub(crate) enum LineResult {
    Line(String),
    Eof, // Ctrl-C / Ctrl-D → exit
}

/// Commands from `commands` whose name starts with `prefix` (populates the dropdown).
fn matches_in<'a>(
    commands: &'a [(&'a str, &'a str)],
    prefix: &str,
) -> Vec<&'a (&'a str, &'a str)> {
    if prefix.contains(' ') {
        return vec![];
    }
    commands.iter().filter(|(cmd, _)| cmd.starts_with(prefix)).collect()
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run_shell(config: AppConfig) -> Result<()> {
    print_banner();

    let mut history: Vec<String> = load_history();
    let mut reader = PaletteReader::new(PROMPT, COMMANDS, (0x7C, 0xE3, 0x8B));

    loop {
        match reader.read_line(&history)? {
            LineResult::Eof => break,
            LineResult::Line(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if history.last().map(|h| h != &line).unwrap_or(true) {
                    history.push(line.clone());
                }
                match dispatch(&line, &config).await {
                    Ok(true) => break, // /quit
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("{}", format!("Error: {e}").red());
                        let msg = e.to_string();
                        if msg.contains("not running") || msg.contains("connect") {
                            eprintln!("{}", "  Hint: run `agenta daemon start` first".dimmed());
                        }
                    }
                }
            }
        }
    }

    // Last meaningful command (skip the quit command that triggered the exit).
    let last_cmd = history
        .iter()
        .rev()
        .find(|c| !matches!(c.as_str(), "/quit" | "/exit" | "/q"))
        .cloned();
    save_history(&history);
    println!();
    println!("{}", "  👋  Until next time — your agents keep running in the background.".dimmed());
    if let Some(cmd) = last_cmd {
        println!();
        println!("  {} {}", "Last shell execution:".dimmed(), cmd);
    }
    Ok(())
}

// ── Live palette line reader ────────────────────────────────────────────────────
//
// Reads a single line in raw mode. While the buffer starts with `/`, a dropdown
// of matching commands renders directly below the prompt and filters as you type;
// ↑/↓ move the highlight, Enter/Tab runs the highlighted command. When the buffer
// isn't a slash-command, it behaves like an ordinary prompt (↑/↓ = history).

pub(crate) struct PaletteReader {
    prompt: &'static str,
    commands: &'static [(&'static str, &'static str)],
    prompt_rgb: (u8, u8, u8),
    buf: String,
    cursor: usize,    // char index into buf
    selected: usize,  // index into the current matches
    cursor_row: u16,  // row offset of the cursor below the prompt's first row (last render)
    hist_idx: Option<usize>,
    /// Multi-line pastes, held aside while the buffer shows a short placeholder.
    /// Expanded back into the line on submit. Keeps a 50-line paste from flooding
    /// the prompt while still sending the whole thing.
    pastes: Vec<String>,
}

impl PaletteReader {
    /// `prompt` is the plain prompt text (rendered in `prompt_rgb`); `commands`
    /// populates the `/` dropdown.
    pub(crate) fn new(
        prompt: &'static str,
        commands: &'static [(&'static str, &'static str)],
        prompt_rgb: (u8, u8, u8),
    ) -> Self {
        Self {
            prompt,
            commands,
            prompt_rgb,
            buf: String::new(),
            cursor: 0,
            selected: 0,
            cursor_row: 0,
            hist_idx: None,
            pastes: Vec::new(),
        }
    }

    pub(crate) fn read_line(&mut self, history: &[String]) -> Result<LineResult> {
        self.buf.clear();
        self.cursor = 0;
        self.selected = 0;
        self.cursor_row = 0;
        self.hist_idx = None;
        self.pastes.clear();

        enable_raw_mode()?;
        // Ask the terminal to bracket pasted text. Without this a paste is just a
        // fast burst of keys, so every newline reads as Enter and each line gets
        // submitted as its own message.
        let _ = execute!(stdout(), EnableBracketedPaste);
        let result = self.event_loop(history);
        let _ = execute!(stdout(), DisableBracketedPaste);
        let _ = disable_raw_mode();
        result
    }

    /// Marker shown in the buffer in place of a multi-line paste.
    fn paste_marker(index: usize, lines: usize) -> String {
        format!("[Pasted text #{} +{} lines]", index, lines)
    }

    /// Insert pasted text at the cursor.
    ///
    /// A single-line paste is indistinguishable from typing, so it goes in as-is.
    /// A multi-line paste is stashed and represented by a short marker instead:
    /// dropping 50 raw lines into a one-line prompt is unreadable, and the marker
    /// makes it obvious the whole block is queued as ONE message rather than one
    /// message per line. `expand_pastes` puts the real text back on submit.
    fn insert_paste(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.is_empty() {
            return;
        }

        let line_count = normalized.lines().count();
        let insert = if line_count > 1 {
            self.pastes.push(normalized);
            Self::paste_marker(self.pastes.len(), line_count)
        } else {
            normalized
        };

        let idx = self.byte_idx();
        self.buf.insert_str(idx, &insert);
        self.cursor += insert.chars().count();
        self.selected = 0;
        self.hist_idx = None;
    }

    /// Swap any paste markers back for the text they stand in for.
    fn expand_pastes(&self, line: &str) -> String {
        let mut out = line.to_string();
        for (i, paste) in self.pastes.iter().enumerate() {
            let marker = Self::paste_marker(i + 1, paste.lines().count());
            out = out.replace(&marker, paste);
        }
        out
    }

    fn event_loop(&mut self, history: &[String]) -> Result<LineResult> {
        self.render()?;
        loop {
            let ev = match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => k,
                Event::Paste(text) => {
                    self.insert_paste(&text);
                    self.render()?;
                    continue;
                }
                Event::Resize(..) => {
                    self.render()?;
                    continue;
                }
                _ => continue,
            };

            let menu = matches_in(self.commands, &self.buf);
            let menu_open = self.buf.starts_with('/') && !menu.is_empty();

            match ev.code {
                KeyCode::Char('c') if ev.modifiers.contains(KeyModifiers::CONTROL) => {
                    return self.finish_eof();
                }
                KeyCode::Char('d') if ev.modifiers.contains(KeyModifiers::CONTROL)
                    && self.buf.is_empty() =>
                {
                    return self.finish_eof();
                }
                KeyCode::Char(c) => {
                    let idx = self.byte_idx();
                    self.buf.insert(idx, c);
                    self.cursor += 1;
                    self.selected = 0;
                    self.hist_idx = None;
                }
                KeyCode::Backspace => {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        let idx = self.byte_idx();
                        self.buf.remove(idx);
                        self.selected = 0;
                        self.hist_idx = None;
                    }
                }
                KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Right => {
                    if self.cursor < self.buf.chars().count() {
                        self.cursor += 1;
                    }
                }
                KeyCode::Up => {
                    if menu_open {
                        self.selected = self.selected.saturating_sub(1);
                    } else {
                        self.history_prev(history);
                    }
                }
                KeyCode::Down => {
                    if menu_open {
                        if self.selected + 1 < menu.len() {
                            self.selected += 1;
                        }
                    } else {
                        self.history_next(history);
                    }
                }
                KeyCode::Esc => {
                    self.buf.clear();
                    self.cursor = 0;
                    self.selected = 0;
                }
                KeyCode::Tab => {
                    if menu_open {
                        // Complete the buffer to the highlighted command (don't run yet).
                        let chosen = menu[self.selected.min(menu.len() - 1)].0;
                        self.buf = chosen.to_string();
                        self.cursor = self.buf.chars().count();
                        self.selected = 0;
                    }
                }
                KeyCode::Enter => {
                    let line = if menu_open {
                        menu[self.selected.min(menu.len() - 1)].0.to_string()
                    } else {
                        self.expand_pastes(&self.buf)
                    };
                    return self.finish_line(line);
                }
                _ => {}
            }

            self.render()?;
        }
    }

    fn byte_idx(&self) -> usize {
        self.buf
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.buf.len())
    }

    fn history_prev(&mut self, history: &[String]) {
        if history.is_empty() {
            return;
        }
        let next = match self.hist_idx {
            None => history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.hist_idx = Some(next);
        self.set_buf(history[next].clone());
    }

    fn history_next(&mut self, history: &[String]) {
        match self.hist_idx {
            Some(i) if i + 1 < history.len() => {
                self.hist_idx = Some(i + 1);
                self.set_buf(history[i + 1].clone());
            }
            Some(_) => {
                self.hist_idx = None;
                self.set_buf(String::new());
            }
            None => {}
        }
    }

    fn set_buf(&mut self, s: String) {
        self.cursor = s.chars().count();
        self.buf = s;
        self.selected = 0;
    }

    /// Clear the dropdown, commit the prompt line into scrollback, return the line.
    fn finish_line(&mut self, line: String) -> Result<LineResult> {
        let mut out = stdout();
        self.clear_all(&mut out)?;
        let (r, g, b) = self.prompt_rgb;
        write!(out, "{}{}\r\n", self.prompt.truecolor(r, g, b), self.buf)?;
        out.flush()?;
        Ok(LineResult::Line(line))
    }

    fn finish_eof(&mut self) -> Result<LineResult> {
        let mut out = stdout();
        self.clear_all(&mut out)?;
        write!(out, "\r\n")?;
        out.flush()?;
        Ok(LineResult::Eof)
    }

    /// Return to the prompt's first row (the cursor may be several rows down when
    /// the input wrapped or the dropdown is open) and wipe everything below it.
    fn clear_all(&mut self, out: &mut impl Write) -> Result<()> {
        if self.cursor_row > 0 {
            queue!(out, cursor::MoveUp(self.cursor_row))?;
        }
        queue!(out, cursor::MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        self.cursor_row = 0;
        Ok(())
    }

    fn render(&mut self) -> Result<()> {
        let mut out = stdout();
        let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80).max(1);
        let (r, g, b) = self.prompt_rgb;

        // Go back to the prompt's first row and clear the whole region we drew.
        self.clear_all(&mut out)?;

        // Prompt + buffer (the terminal wraps long input automatically).
        write!(out, "{}{}", self.prompt.truecolor(r, g, b), self.buf)?;

        let menu = matches_in(self.commands, &self.buf);
        let menu_open = self.buf.starts_with('/') && !menu.is_empty();

        // When the input fills exactly to the right edge, terminals leave the cursor
        // in a "pending wrap" at the end of the row instead of advancing to the next
        // line. Our row math (below) counts it as a new row, so without forcing the
        // advance the stored cursor_row is one too high and the next clear_all()
        // MoveUp overshoots — shoving the earlier lines upward. An explicit newline
        // makes the terminal's position and our bookkeeping agree.
        let prompt_cols = self.prompt.chars().count();
        let input_cols = prompt_cols + self.buf.chars().count();
        if !menu_open && !self.buf.is_empty() && input_cols % width == 0 {
            write!(out, "\r\n")?;
        }

        if menu_open {
            for (i, (cmd, desc)) in menu.iter().enumerate() {
                write!(out, "\r\n")?;
                let label = format!("  {:<16} {}", cmd, desc);
                if i == self.selected.min(menu.len() - 1) {
                    queue!(out, SetAttribute(Attribute::Reverse))?;
                    write!(out, "{}", label)?;
                    queue!(out, SetAttribute(Attribute::Reset))?;
                } else {
                    write!(out, "{}", label.dimmed())?;
                }
            }
        }

        // Row bookkeeping (rows are offsets below the prompt's first row).
        let prompt_len = self.prompt.chars().count();
        let input_rows = ((prompt_len + self.buf.chars().count()) / width) as u16;
        let menu_rows = if menu_open { menu.len() as u16 } else { 0 };
        // The cursor is currently at the end of everything we wrote.
        let end_row = if menu_open { input_rows + menu_rows } else { input_rows };

        // Move it to the text cursor's position (accounting for wrap).
        let cursor_total = prompt_len + self.cursor;
        let cur_row = (cursor_total / width) as u16;
        let cur_col = (cursor_total % width) as u16;
        let up = end_row.saturating_sub(cur_row);
        if up > 0 {
            queue!(out, cursor::MoveUp(up))?;
        }
        queue!(out, cursor::MoveToColumn(cur_col))?;
        self.cursor_row = cur_row;

        out.flush()?;
        Ok(())
    }
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

pub(crate) async fn dispatch(input: &str, config: &AppConfig) -> Result<bool> {
    match input {
        "/quit" | "/exit" | "/q" => return Ok(true),
        "/help" | "/h"           => print_help(),
        "/list"                  => cmd_list(config).await?,
        "/list-tools"            => cmd_list_tools(config).await?,
        "/list-proposals"        => super::commands::list_proposals(config, false).await?,
        "/list-kb"               => cmd_list_kb(config).await?,
        "/attach-kb"             => picker_attach_kb(config).await?,
        "/detach-kb"             => picker_detach_kb(config).await?,
        "/list-scripts"          => println!("{}", "Scripts coming soon.".dimmed()),
        "/status"                => cmd_status(config).await?,
        "/create-agent"          => wizard_create_agent(config).await?,
        "/update-agent"          => wizard_update_agent(config).await?,
        "/create-tool"           => wizard_create_tool(config).await?,
        "/update-tool"           => println!("{}", "Coming soon.".dimmed()),
        "/create-script"         => println!("{}", "Scripts coming soon.".dimmed()),
        "/get"                   => picker_get(config).await?,
        "/get-tool"              => picker_get_tool(config).await?,
        "/delete"                => picker_delete(config).await?,
        "/run"                   => picker_run(config).await?,
        "/stop"                  => picker_stop(config).await?,
        "/logs"                  => picker_logs(config).await?,
        _ if input.starts_with('/') => {
            println!("Unknown command: {}. Type {} for available commands.", input.yellow(), "/help".cyan());
        }
        _ => {
            println!("Commands start with /. Type {} for available commands.", "/help".cyan());
        }
    }
    Ok(false)
}

// ── Banner + help ─────────────────────────────────────────────────────────────

fn print_banner() {
    println!();
    println!("  {} {}", "agenta".bold().green(), "interactive shell".dimmed());
    println!("  Type {} for commands, {} to exit.", "/help".cyan(), "/quit".cyan());
    println!();
}

fn print_help() {
    use comfy_table::{Cell, Table};

    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_header(vec![
        Cell::new("Command").add_attribute(comfy_table::Attribute::Bold),
        Cell::new("Description").add_attribute(comfy_table::Attribute::Bold),
    ]);

    for (cmd, desc) in COMMANDS {
        table.add_row(vec![
            Cell::new(cmd).fg(comfy_table::Color::Cyan),
            Cell::new(desc),
        ]);
    }

    println!("{table}");
    println!("{}", "  Tip: type / and press Tab to browse commands.".dimmed());
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn history_file() -> Option<String> {
    dirs::data_dir()
        .map(|d| d.join("agenta").join("shell_history"))
        .and_then(|p| {
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            p.to_str().map(|s| s.to_string())
        })
}

/// Load shell history (one command per line), capped to the last 1000 entries.
fn load_history() -> Vec<String> {
    let Some(path) = history_file() else { return vec![] };
    let Ok(content) = std::fs::read_to_string(&path) else { return vec![] };
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    if lines.len() > 1000 {
        lines = lines.split_off(lines.len() - 1000);
    }
    lines
}

fn save_history(history: &[String]) {
    if let Some(path) = history_file() {
        let tail = if history.len() > 1000 { &history[history.len() - 1000..] } else { history };
        let _ = std::fs::write(path, tail.join("\n"));
    }
}

async fn fetch_agents(config: &AppConfig) -> Result<Vec<serde_json::Value>> {
    match daemon_request(config, DaemonRequest::ListAgents).await? {
        DaemonResponse::AgentList { agents } => Ok(agents),
        DaemonResponse::Error { message } => Err(anyhow::anyhow!("{}", message)),
        _ => Ok(vec![]),
    }
}

async fn fetch_tools(config: &AppConfig) -> Result<Vec<serde_json::Value>> {
    match daemon_request(config, DaemonRequest::ListTools).await? {
        DaemonResponse::ToolList { tools } => Ok(tools),
        DaemonResponse::Error { message } => Err(anyhow::anyhow!("{}", message)),
        _ => Ok(vec![]),
    }
}

/// Knowledge bases live in Postgres/pgvector, queried directly (not via the daemon),
/// same as the `agenta knowledge` CLI. Returns a friendly error if RAG isn't configured.
async fn fetch_kbs(config: &AppConfig) -> Result<Vec<crate::knowledge::KnowledgeBase>> {
    use crate::knowledge::VectorStore;
    let url = match &config.database_url {
        Some(u) if u.starts_with("postgres") => u.clone(),
        _ => return Err(anyhow::anyhow!(
            "RAG needs Postgres — set database_url in config.toml (with pgvector)."
        )),
    };
    let store = crate::knowledge::PgVectorStore::new(&url).await?;
    Ok(store.list_kbs().await?)
}

/// Fetch one agent's full JSON (so we can mutate its config and send it back whole,
/// mirroring how `agenta update` applies KB changes).
async fn fetch_agent(config: &AppConfig, id: &str) -> Result<serde_json::Value> {
    match daemon_request(config, DaemonRequest::GetAgent { id: id.to_string() }).await? {
        DaemonResponse::AgentDetails { agent } => Ok(agent),
        DaemonResponse::Error { message } => Err(anyhow::anyhow!("{}", message)),
        _ => Err(anyhow::anyhow!("Unexpected response")),
    }
}

async fn cmd_list_kb(config: &AppConfig) -> Result<()> {
    let kbs = match fetch_kbs(config).await {
        Ok(k) => k,
        Err(e) => { println!("{}", e.to_string().yellow()); return Ok(()); }
    };
    if kbs.is_empty() {
        println!("{}", "No knowledge bases. Create one: agenta knowledge create <name>".dimmed());
        return Ok(());
    }
    let mut table = styled_table();
    table.set_header(vec!["NAME", "EMBEDDER", "DIM"]);
    for kb in &kbs {
        table.add_row(vec![kb.name.as_str(), kb.embedder.as_str(), &kb.dimension.to_string()]);
    }
    println!("{table}");
    println!("{}", "  Ingest files with: agenta knowledge add <kb> <file>".dimmed());
    Ok(())
}

async fn picker_attach_kb(config: &AppConfig) -> Result<()> {
    use inquire::Select;

    let agents = fetch_agents(config).await?;
    if agents.is_empty() { println!("{}", "No agents found.".dimmed()); return Ok(()); }
    let kbs = match fetch_kbs(config).await {
        Ok(k) => k,
        Err(e) => { println!("{}", e.to_string().yellow()); return Ok(()); }
    };
    if kbs.is_empty() {
        println!("{}", "No knowledge bases yet. Create one: agenta knowledge create <name>".dimmed());
        return Ok(());
    }

    let names = agent_names(&agents);
    let agent_name = Select::new("Attach to agent:", names.iter().map(|s| s.as_str()).collect()).prompt()?;
    let kb = Select::new("Knowledge base:", kbs.iter().map(|k| k.name.as_str()).collect()).prompt()?;

    let mut agent = fetch_agent(config, agent_name).await?;
    let mut list = agent["config"]["knowledge_bases"].as_array().cloned().unwrap_or_default();
    if list.iter().any(|v| v.as_str() == Some(kb)) {
        println!("{}", format!("'{}' is already attached to {}.", kb, agent_name).yellow());
        return Ok(());
    }
    list.push(serde_json::Value::String(kb.to_string()));
    agent["config"]["knowledge_bases"] = serde_json::Value::Array(list);

    let id = agent["id"].as_str().unwrap_or("").to_string();
    match daemon_request(config, DaemonRequest::UpdateAgent { id, agent }).await? {
        DaemonResponse::AgentDetails { .. } =>
            println!("{} Attached '{}' to {}", "✓".green(), kb.bold(), agent_name.bold()),
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }
    Ok(())
}

async fn picker_detach_kb(config: &AppConfig) -> Result<()> {
    use inquire::Select;

    let agents = fetch_agents(config).await?;
    if agents.is_empty() { println!("{}", "No agents found.".dimmed()); return Ok(()); }
    let names = agent_names(&agents);
    let agent_name = Select::new("Detach from agent:", names.iter().map(|s| s.as_str()).collect()).prompt()?;

    let mut agent = fetch_agent(config, agent_name).await?;
    let attached: Vec<String> = agent["config"]["knowledge_bases"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if attached.is_empty() {
        println!("{}", format!("{} has no knowledge bases attached.", agent_name).dimmed());
        return Ok(());
    }
    let kb = Select::new("Detach which KB:", attached.iter().map(|s| s.as_str()).collect()).prompt()?;
    let new_list: Vec<serde_json::Value> = attached
        .iter()
        .filter(|k| k.as_str() != kb)
        .map(|k| serde_json::Value::String(k.clone()))
        .collect();
    agent["config"]["knowledge_bases"] = serde_json::Value::Array(new_list);

    let id = agent["id"].as_str().unwrap_or("").to_string();
    match daemon_request(config, DaemonRequest::UpdateAgent { id, agent }).await? {
        DaemonResponse::AgentDetails { .. } =>
            println!("{} Detached '{}' from {}", "✓".green(), kb.bold(), agent_name.bold()),
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }
    Ok(())
}

fn agent_names(agents: &[serde_json::Value]) -> Vec<String> {
    agents.iter()
        .filter_map(|a| a["name"].as_str().map(|s| s.to_string()))
        .collect()
}

fn tool_names(tools: &[serde_json::Value]) -> Vec<String> {
    tools.iter()
        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
        .collect()
}

// ── Simple commands ───────────────────────────────────────────────────────────

async fn cmd_list(config: &AppConfig) -> Result<()> {
    let agents = fetch_agents(config).await?;
    if agents.is_empty() {
        println!("{}", "No agents found. Use /create-agent to create one.".dimmed());
        return Ok(());
    }
    let mut table = styled_table();
    table.set_header(vec!["NAME", "MODEL", "STATUS", "PROVIDER", "MODE"]);
    for agent in &agents {
        table.add_row(vec![
            agent["name"].as_str().unwrap_or("-"),
            agent["model"].as_str().unwrap_or("-"),
            agent["status"].as_str().unwrap_or("-"),
            agent["provider"].as_str().unwrap_or("ollama"),
            agent["execution_mode"].as_str().unwrap_or("-"),
        ]);
    }
    println!("{table}");
    Ok(())
}

async fn cmd_list_tools(config: &AppConfig) -> Result<()> {
    let tools = fetch_tools(config).await?;
    if tools.is_empty() {
        println!("{}", "No tools found. Use /create-tool to create one.".dimmed());
        return Ok(());
    }
    // Keep the DESCRIPTION column compact — truncate long descriptions with an ellipsis
    // so a single verbose tool doesn't blow out the whole table width.
    const DESC_MAX: usize = 50;
    let mut table = styled_table();
    table.set_header(vec!["NAME", "DESCRIPTION", "ENABLED"]);
    for tool in &tools {
        let desc = tool["description"].as_str().unwrap_or("-");
        let desc = if desc.chars().count() > DESC_MAX {
            format!("{}…", desc.chars().take(DESC_MAX - 1).collect::<String>().trim_end())
        } else {
            desc.to_string()
        };
        table.add_row(vec![
            tool["name"].as_str().unwrap_or("-").to_string(),
            desc,
            if tool["enabled"].as_bool().unwrap_or(true) { "yes".to_string() } else { "no".to_string() },
        ]);
    }
    println!("{table}");
    Ok(())
}

async fn cmd_status(config: &AppConfig) -> Result<()> {
    match daemon_request(config, DaemonRequest::Ping).await {
        Ok(DaemonResponse::Status { running, version, .. }) => {
            if running {
                println!("  {} Daemon running  (v{})", "●".green(), version);
            } else {
                println!("  {} Daemon not running", "●".red());
                println!("  {}", "Run: agenta daemon start".dimmed());
            }
        }
        _ => {
            println!("  {} Daemon not running", "●".red());
            println!("  {}", "Run: agenta daemon start".dimmed());
        }
    }
    Ok(())
}

// ── Wizards ───────────────────────────────────────────────────────────────────

async fn wizard_create_agent(config: &AppConfig) -> Result<()> {
    use inquire::{Confirm, Select, Text};

    println!("{}", "  Creating a new agent...".dimmed());

    let name = Text::new("Name:").prompt()?;
    if name.trim().is_empty() {
        return Err(anyhow::anyhow!("Name cannot be empty"));
    }

    let model = Text::new("Model:").with_default("llama2").prompt()?;

    let provider = Select::new("Provider:", vec!["ollama", "deepseek", "openrouter", "openai"])
        .with_starting_cursor(0)
        .prompt()?;

    let prompt_text = Text::new("System prompt:").prompt()?;

    let mode = Select::new("Execution mode:", vec!["once", "scheduled", "continuous"])
        .with_starting_cursor(0)
        .prompt()?;

    let schedule = if mode == "scheduled" {
        let s = Text::new("Cron schedule (e.g. 0 8 * * *):").prompt()?;
        Some(s)
    } else {
        None
    };

    let memory = Confirm::new("Enable memory?").with_default(false).prompt()?;
    let deep = Confirm::new("Enable deep agent mode?").with_default(false).prompt()?;

    let deep_iterations = if deep {
        let s = Text::new("Max iterations:").with_default("10").prompt()?;
        s.parse::<u32>().unwrap_or(10)
    } else {
        10
    };

    // Build agent JSON
    let mut agent = serde_json::json!({
        "name": name.trim(),
        "model": model.trim(),
        "system_prompt": prompt_text,
        "execution_mode": mode,
        "memory_enabled": memory,
        "provider": if provider == "ollama" { serde_json::Value::Null } else { serde_json::Value::String(provider.to_string()) },
        "deep_agent": deep,
        "deep_agent_config": {
            "max_iterations": deep_iterations,
        },
    });

    if let Some(s) = schedule {
        agent["schedule"] = serde_json::Value::String(s);
    }

    match daemon_request(config, DaemonRequest::CreateAgent { agent }).await? {
        DaemonResponse::AgentDetails { agent } => {
            let name = agent["name"].as_str().unwrap_or("unknown");
            println!("{} Agent '{}' created.", "✓".green(), name.bold());
        }
        DaemonResponse::Error { message } => {
            return Err(anyhow::anyhow!("{}", message));
        }
        _ => {}
    }

    Ok(())
}

async fn wizard_update_agent(config: &AppConfig) -> Result<()> {
    use inquire::{Confirm, Select, Text};

    let agents = fetch_agents(config).await?;
    if agents.is_empty() {
        println!("{}", "No agents found.".dimmed());
        return Ok(());
    }

    let names = agent_names(&agents);
    let selected = Select::new("Select agent to update:", names.iter().map(|s| s.as_str()).collect()).prompt()?;
    let agent = agents.iter().find(|a| a["name"].as_str() == Some(selected)).unwrap();
    let agent_id = agent["id"].as_str().unwrap_or("").to_string();

    println!("{}", "  Leave blank to keep current value.".dimmed());

    let new_model = Text::new("Model:")
        .with_default(agent["model"].as_str().unwrap_or(""))
        .prompt()?;

    let new_prompt = Text::new("System prompt:")
        .with_default(agent["system_prompt"].as_str().unwrap_or(""))
        .prompt()?;

    let providers = vec!["ollama", "deepseek", "openrouter", "openai"];
    let current_provider = agent["provider"].as_str().unwrap_or("ollama");
    let provider_idx = providers.iter().position(|&p| p == current_provider).unwrap_or(0);
    let new_provider = Select::new("Provider:", providers)
        .with_starting_cursor(provider_idx)
        .prompt()?;

    let new_memory = Confirm::new("Enable memory?")
        .with_default(agent["memory_enabled"].as_bool().unwrap_or(false))
        .prompt()?;

    let updates = serde_json::json!({
        "model": new_model.trim(),
        "system_prompt": new_prompt,
        "provider": if new_provider == "ollama" { serde_json::Value::Null } else { serde_json::Value::String(new_provider.to_string()) },
        "memory_enabled": new_memory,
    });

    match daemon_request(config, DaemonRequest::UpdateAgent { id: agent_id, agent: updates }).await? {
        DaemonResponse::AgentDetails { agent } => {
            println!("{} Agent '{}' updated.", "✓".green(), agent["name"].as_str().unwrap_or("").bold());
        }
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }

    Ok(())
}

/// Read a multi-line value like a heredoc: type or paste across as many lines as you
/// want (blank lines kept, so paragraphs work), then press Ctrl-D on a new line to
/// finish. Better than a single-line prompt for descriptions written in a terminal.
fn prompt_multiline(label: &str) -> Result<String> {
    use std::io::BufRead;
    println!("{}", label.bold());
    println!("{}", "  (write across multiple lines; press Ctrl-D when done)".dimmed());
    let stdin = std::io::stdin();
    let mut out = String::new();
    for line in stdin.lock().lines() {
        out.push_str(&line?);
        out.push('\n');
    }
    Ok(out.trim_end().to_string())
}

async fn wizard_create_tool(config: &AppConfig) -> Result<()> {
    use inquire::{Confirm, Select, Text};

    println!("{}", "  Creating a new tool...".dimmed());

    let name = Text::new("Tool name:").prompt()?;
    if name.trim().is_empty() {
        return Err(anyhow::anyhow!("Name cannot be empty"));
    }

    let description = prompt_multiline("Description:")?;

    let handler = Text::new("Handler script path (leave blank to auto-scaffold):").prompt()?;

    // Resolve the handler: an explicit path, an auto-scaffolded starter script, or none.
    let resolved_handler = if handler.trim().is_empty() {
        let scaffold = Confirm::new("Auto-generate starter script?")
            .with_default(true)
            .prompt()?;
        if scaffold {
            Some(super::commands::scaffold_tool_handler(name.trim(), None)?)
        } else {
            None
        }
    } else {
        Some(handler.trim().to_string())
    };

    // Effect classification — drives the confirm-before-run guard for risky tools.
    let side_effect = match Select::new(
        "Effect on the world:",
        vec![
            "read-only  (no state change — compute, search, read)",
            "write      (mutates state / sends to an external system)",
            "destructive(irreversible — delete, deploy, transfer)",
        ],
    )
    .prompt()?
    {
        s if s.starts_with("write") => crate::core::SideEffect::Write,
        s if s.starts_with("destructive") => crate::core::SideEffect::Destructive,
        _ => crate::core::SideEffect::ReadOnly,
    };

    // Secret allowlist — the only env vars the handler will receive.
    let secrets_raw = Text::new("Secrets allowlist (env var names, comma-separated; blank for none):")
        .prompt()?;
    let secrets: Vec<String> = secrets_raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Build a full ToolResource (this generates the id + timestamps the daemon needs).
    let mut tool = crate::core::ToolResource::new(
        name.trim().to_string(),
        description,
        serde_json::json!({ "type": "object", "properties": {}, "required": [] }),
        resolved_handler,
    );
    tool.side_effect = side_effect;
    tool.secrets = secrets;

    match daemon_request(
        config,
        DaemonRequest::CreateTool { tool: serde_json::to_value(&tool)? },
    )
    .await?
    {
        DaemonResponse::Success { message } => println!("{}", message.green()),
        DaemonResponse::ToolDetails { tool } => {
            println!("{} Tool '{}' created.", "✓".green(), tool["name"].as_str().unwrap_or("").bold());
        }
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }

    Ok(())
}

// ── Pickers ───────────────────────────────────────────────────────────────────

async fn picker_get(config: &AppConfig) -> Result<()> {
    use inquire::Select;

    let agents = fetch_agents(config).await?;
    if agents.is_empty() { println!("{}", "No agents found.".dimmed()); return Ok(()); }

    let names = agent_names(&agents);
    let selected = Select::new("Select agent:", names.iter().map(|s| s.as_str()).collect()).prompt()?;

    match daemon_request(config, DaemonRequest::GetAgent { id: selected.to_string() }).await? {
        DaemonResponse::AgentDetails { agent } => print_agent_detail(&agent),
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }

    Ok(())
}

async fn picker_delete(config: &AppConfig) -> Result<()> {
    use inquire::{Confirm, Select};

    let agents = fetch_agents(config).await?;
    if agents.is_empty() { println!("{}", "No agents found.".dimmed()); return Ok(()); }

    let names = agent_names(&agents);
    let selected = Select::new("Select agent to delete:", names.iter().map(|s| s.as_str()).collect()).prompt()?;

    let confirmed = Confirm::new(&format!("Delete '{}'? This cannot be undone.", selected))
        .with_default(false)
        .prompt()?;

    if !confirmed {
        println!("Cancelled.");
        return Ok(());
    }

    match daemon_request(config, DaemonRequest::DeleteAgent { id: selected.to_string() }).await? {
        DaemonResponse::Success { message } => println!("{} {}", "✓".green(), message),
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }

    Ok(())
}

async fn picker_run(config: &AppConfig) -> Result<()> {
    use inquire::{Select, Text};

    let agents = fetch_agents(config).await?;
    if agents.is_empty() { println!("{}", "No agents found.".dimmed()); return Ok(()); }

    let names = agent_names(&agents);
    let selected = Select::new("Select agent:", names.iter().map(|s| s.as_str()).collect()).prompt()?;

    let input = Text::new("Input (optional):").prompt()?;
    let input = if input.trim().is_empty() { None } else { Some(input) };

    match daemon_request(config, DaemonRequest::RunAgent {
        id: selected.to_string(),
        input,
    }).await? {
        DaemonResponse::ExecutionStarted { execution_id } => {
            println!("{} Agent started. Execution ID: {}", "✓".green(), execution_id.dimmed());
            wait_and_print_result(config, &execution_id).await?;
        }
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }

    Ok(())
}

/// Poll an execution until it reaches a terminal state, then print its output.
async fn wait_and_print_result(config: &AppConfig, execution_id: &str) -> Result<()> {
    use std::time::Duration;

    print!("{}", "  Running".dimmed());
    let _ = std::io::Write::flush(&mut std::io::stdout());

    // ~5 min ceiling (375 * 800ms); agents that run longer can be checked via /logs.
    for _ in 0..375 {
        tokio::time::sleep(Duration::from_millis(800)).await;
        print!("{}", ".".dimmed());
        let _ = std::io::Write::flush(&mut std::io::stdout());

        let result = match daemon_request(
            config,
            DaemonRequest::GetExecution { id: execution_id.to_string() },
        )
        .await?
        {
            DaemonResponse::ExecutionResult { result } => result,
            DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
            _ => continue,
        };

        let status = result["status"].as_str().unwrap_or("running");
        match status {
            "completed" => {
                println!();
                if let Some(output) = result["output"].as_str() {
                    println!("\n{}\n{}", "Result:".bold().green(), output);
                } else {
                    println!("{}", "Completed (no output).".dimmed());
                }
                return Ok(());
            }
            "failed" | "cancelled" => {
                println!();
                let err = result["error"].as_str().unwrap_or(status);
                println!("{} {}", "✗".red(), err);
                return Ok(());
            }
            _ => {}
        }
    }

    println!();
    println!("{}", "Still running — check /logs for the result.".dimmed());
    Ok(())
}

async fn picker_stop(config: &AppConfig) -> Result<()> {
    use inquire::Select;

    let agents = fetch_agents(config).await?;
    if agents.is_empty() { println!("{}", "No agents found.".dimmed()); return Ok(()); }

    let names = agent_names(&agents);
    let selected = Select::new("Select agent to stop:", names.iter().map(|s| s.as_str()).collect()).prompt()?;

    match daemon_request(config, DaemonRequest::StopAgent { id: selected.to_string() }).await? {
        DaemonResponse::Success { message } => println!("{} {}", "✓".green(), message),
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }

    Ok(())
}

async fn picker_logs(config: &AppConfig) -> Result<()> {
    use inquire::Select;

    let agents = fetch_agents(config).await?;
    if agents.is_empty() { println!("{}", "No agents found.".dimmed()); return Ok(()); }

    let names = agent_names(&agents);
    let selected = Select::new("Select agent:", names.iter().map(|s| s.as_str()).collect()).prompt()?;

    // Resolve the chosen agent's id so we can filter its executions.
    let agent_id = agents.iter()
        .find(|a| a["name"].as_str() == Some(selected))
        .and_then(|a| a["id"].as_str())
        .map(|s| s.to_string());

    // Pull recent executions (global) and keep only this agent's. They come newest-first.
    let mut execs: Vec<serde_json::Value> =
        match daemon_request(config, DaemonRequest::ListExecutions { limit: 100 }).await? {
            DaemonResponse::ExecutionList { executions } => executions,
            DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
            _ => vec![],
        };
    if let Some(ref aid) = agent_id {
        execs.retain(|e| e["agent_id"].as_str() == Some(aid.as_str()));
    }
    if execs.is_empty() {
        println!("{}", "No executions found for this agent.".dimmed());
        return Ok(());
    }

    // One execution → show it directly; many → let the user pick (latest at top).
    let exec = if execs.len() == 1 {
        &execs[0]
    } else {
        let labels: Vec<String> = execs.iter().map(exec_label).collect();
        let chosen = Select::new("Select execution:", labels.clone()).prompt()?;
        let idx = labels.iter().position(|l| l == &chosen).unwrap_or(0);
        &execs[idx]
    };

    print_execution_detail(exec);
    Ok(())
}

/// Short one-line label for an execution menu entry: `[2026-06-30 09:47:00] dc688538 - completed`
fn exec_label(e: &serde_json::Value) -> String {
    let ts = e["started_at"].as_str().unwrap_or("");
    let ts = ts.get(..19).unwrap_or(ts).replace('T', " ");
    let id8: String = e["id"].as_str().unwrap_or("").chars().take(8).collect();
    let status = e["status"].as_str().unwrap_or("unknown");
    format!("[{}] {} - {}", ts, id8, status)
}

/// Full detail for a single execution, including its output.
fn print_execution_detail(e: &serde_json::Value) {
    let fmt_ts = |s: &str| s.get(..19).unwrap_or(s).replace('T', " ");
    println!();
    if let Some(id) = e["id"].as_str() {
        println!("  {:11} {}", "Execution:".bold(), id);
    }
    if let Some(s) = e["started_at"].as_str() {
        println!("  {:11} {}", "Started:".bold(), fmt_ts(s));
    }
    if let Some(s) = e["completed_at"].as_str() {
        println!("  {:11} {}", "Completed:".bold(), fmt_ts(s));
    }
    if let Some(s) = e["status"].as_str() {
        println!("  {:11} {}", "Status:".bold(), s);
    }
    if let Some(input) = e["input"].as_str() {
        if !input.is_empty() {
            println!("  {:11} {}", "Input:".bold(), input);
        }
    }
    match e["error"].as_str() {
        Some(err) if !err.is_empty() => println!("\n{} {}", "Error:".bold().red(), err),
        _ => {}
    }
    match e["output"].as_str() {
        Some(out) if !out.is_empty() => println!("\n{}\n{}", "Result:".bold().green(), out),
        _ => println!("\n{}", "(no output)".dimmed()),
    }
}

// ── Detail printer ────────────────────────────────────────────────────────────

/// Turn a JSON value into a display string ("-" for null).
fn detail_cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "-".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A Property/Value table that wraps long values to the terminal — matches `agenta
/// tool get`.
fn detail_table() -> comfy_table::Table {
    let mut table = styled_table();
    table.set_header(vec!["Property", "Value"]);
    table
}

fn print_agent_detail(agent: &serde_json::Value) {
    let mut table = detail_table();
    table.add_row(vec!["ID".to_string(), detail_cell(&agent["id"])]);
    table.add_row(vec!["Name".to_string(), detail_cell(&agent["name"])]);
    table.add_row(vec!["Model".to_string(), detail_cell(&agent["model"])]);
    table.add_row(vec!["Provider".to_string(), detail_cell(&agent["provider"])]);
    table.add_row(vec!["Status".to_string(), detail_cell(&agent["status"])]);
    table.add_row(vec!["Mode".to_string(), detail_cell(&agent["execution_mode"])]);
    table.add_row(vec!["Schedule".to_string(), detail_cell(&agent["schedule"])]);
    table.add_row(vec!["Memory".to_string(), detail_cell(&agent["memory_enabled"])]);
    table.add_row(vec![
        "Deep".to_string(),
        if agent["deep_agent_config"].is_object() { "true".to_string() } else { "-".to_string() },
    ]);
    if let Some(kbs) = agent["config"]["knowledge_bases"].as_array() {
        if !kbs.is_empty() {
            let names: Vec<&str> = kbs.iter().filter_map(|v| v.as_str()).collect();
            table.add_row(vec!["Knowledge Bases".to_string(), names.join(", ")]);
        }
    }
    if let Some(prompt) = agent["system_prompt"].as_str() {
        table.add_row(vec!["Prompt".to_string(), prompt.to_string()]);
    }
    println!("{table}");
}

fn print_tool_detail(tool: &serde_json::Value) {
    let mut table = detail_table();
    table.add_row(vec!["ID".to_string(), detail_cell(&tool["id"])]);
    table.add_row(vec!["Name".to_string(), detail_cell(&tool["name"])]);
    table.add_row(vec!["Description".to_string(), detail_cell(&tool["description"])]);
    table.add_row(vec!["Enabled".to_string(), detail_cell(&tool["enabled"])]);
    let side_effect = tool["side_effect"].as_str().unwrap_or("read_only");
    table.add_row(vec!["Side Effect".to_string(), side_effect.to_string()]);
    let secrets = match tool["secrets"].as_array() {
        Some(arr) if !arr.is_empty() => arr
            .iter()
            .filter_map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        _ => "none".to_string(),
    };
    table.add_row(vec!["Secrets".to_string(), secrets]);
    if let Some(http) = tool.get("http").filter(|h| !h.is_null()) {
        let method = http["method"].as_str().unwrap_or("POST");
        table.add_row(vec!["Type".to_string(), format!("HTTP ({method})")]);
        table.add_row(vec!["URL".to_string(), detail_cell(&tool["handler"])]);
        if let Some(headers) = http["headers"].as_object().filter(|h| !h.is_empty()) {
            let hdrs = headers
                .iter()
                .map(|(k, v)| format!("{}: {}", k, v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join("\n");
            table.add_row(vec!["Headers".to_string(), hdrs]);
        }
    } else {
        table.add_row(vec!["Type".to_string(), "script".to_string()]);
        table.add_row(vec!["Handler".to_string(), detail_cell(&tool["handler"])]);
    }
    println!("{table}");
    if let Some(params) = tool.get("parameters").filter(|p| !p.is_null()) {
        println!("\n{}", "Parameters schema:".bold());
        println!("{}", serde_json::to_string_pretty(params).unwrap_or_else(|_| params.to_string()));
    }
}

async fn picker_get_tool(config: &AppConfig) -> Result<()> {
    use inquire::Select;
    let tools = fetch_tools(config).await?;
    if tools.is_empty() {
        println!("{}", "No tools found.".dimmed());
        return Ok(());
    }
    let names = tool_names(&tools);
    let selected =
        Select::new("Select tool:", names.iter().map(|s| s.as_str()).collect()).prompt()?;
    if let Some(tool) = tools.iter().find(|t| t["name"].as_str() == Some(selected)) {
        print_tool_detail(tool);
    }
    Ok(())
}

#[cfg(test)]
mod paste_tests {
    use super::*;

    fn reader() -> PaletteReader {
        PaletteReader::new("> ", &[], (0, 0, 0))
    }

    /// The bug this guards: a multi-line paste used to arrive as a burst of keys
    /// with an Enter per newline, so every line was submitted as its own message
    /// (and its own LLM run). It must become ONE message.
    #[test]
    fn multiline_paste_submits_as_a_single_message() {
        let mut r = reader();
        let pasted = "# PERSONA\nYou are TRAVY.\nBe concise.";
        r.insert_paste(pasted);

        // The prompt stays readable — a marker, not 3 raw lines.
        assert_eq!(r.buf, "[Pasted text #1 +3 lines]");
        // ...but the whole block is what actually gets sent.
        assert_eq!(r.expand_pastes(&r.buf), pasted);
    }

    #[test]
    fn single_line_paste_goes_in_verbatim() {
        let mut r = reader();
        r.insert_paste("just one line");
        assert_eq!(r.buf, "just one line");
        assert_eq!(r.expand_pastes(&r.buf), "just one line");
    }

    /// Windows/editor line endings must not inflate the line count or leak \r.
    #[test]
    fn crlf_is_normalised() {
        let mut r = reader();
        r.insert_paste("one\r\ntwo");
        assert_eq!(r.buf, "[Pasted text #1 +2 lines]");
        assert_eq!(r.expand_pastes(&r.buf), "one\ntwo");
    }

    /// A paste can be surrounded by typed text, and several pastes can coexist.
    #[test]
    fn markers_expand_around_typed_text_and_multiple_pastes() {
        let mut r = reader();
        r.insert_paste("alpha\nbeta");
        r.buf.push_str(" and ");
        r.cursor = r.buf.chars().count();
        r.insert_paste("gamma\ndelta");

        assert_eq!(r.buf, "[Pasted text #1 +2 lines] and [Pasted text #2 +2 lines]");
        assert_eq!(r.expand_pastes(&r.buf), "alpha\nbeta and gamma\ndelta");
    }
}
