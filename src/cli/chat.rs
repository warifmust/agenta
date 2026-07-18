//! Interactive chat with MIND — the default `agenta` experience.
//!
//! A terminal REPL (in the spirit of Claude Code / Codex): you type a request,
//! MIND runs, its reply streams out under a left border, and anything it
//! proposes can be approved inline. MIND stays the builder — it proposes, you
//! approve — so the chat is a conversation, not a command surface.

use anyhow::{anyhow, Result};
use owo_colors::OwoColorize;
use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;

use super::commands::daemon_request;
use super::shell::{dispatch, LineResult, PaletteReader};
use crate::core::{AppConfig, DaemonRequest, DaemonResponse, Proposal, ProposalStatus, Risk};

/// agenta accent — #ff7a45.
const ORANGE: (u8, u8, u8) = (0xff, 0x7a, 0x45);

/// Ping-pong loading frames: a* → a** → a*** → a** → (repeat).
const FRAMES: &[&str] = &["a*", "a**", "a***", "a**"];

/// Slash commands available in the chat — the `/` dropdown. Management commands
/// are handled by the shell dispatcher; `/trace`, `/help`, and exit are chat-local.
const CHAT_COMMANDS: &[(&str, &str)] = &[
    ("/create-agent", "Create a new agent (guided wizard)"),
    ("/update-agent", "Update an existing agent"),
    ("/create-tool", "Create a new tool (guided wizard)"),
    ("/update-tool", "Update an existing tool"),
    ("/list", "List all agents"),
    ("/list-tools", "List all tools"),
    ("/list-proposals", "List pending proposals"),
    ("/list-kb", "List knowledge bases (RAG)"),
    ("/attach-kb", "Attach a knowledge base to an agent"),
    ("/detach-kb", "Detach a knowledge base from an agent"),
    ("/get", "Show agent details"),
    ("/get-tool", "Show tool details"),
    ("/run", "Run an agent"),
    ("/stop", "Stop a running agent"),
    ("/logs", "View agent logs"),
    ("/delete", "Delete an agent"),
    ("/status", "Show daemon status"),
    ("/trace", "Last turn's tool calls + results"),
    ("/help", "Show help"),
    ("/quit", "Exit the chat"),
];

pub async fn run_chat(config: &AppConfig) -> Result<()> {
    print_welcome(config).await;

    // Same palette reader as `agenta shell` — live `/` dropdown + history — but
    // with the chat's prompt, accent, and command set.
    let mut reader = PaletteReader::new("❯ ", CHAT_COMMANDS, ORANGE);
    let mut history: Vec<String> = load_history();

    // The tool calls MIND made last turn — surfaced via `/trace` (the "closed CoT").
    let mut last_trace: Vec<(String, String)> = Vec::new();
    // The MIND conversation so far — each turn is a fresh execution, so we feed
    // the recent transcript back in as context (otherwise MIND has no memory).
    let mut convo: Vec<(String, String)> = Vec::new();

    loop {
        let line = match reader.read_line(&history) {
            Ok(LineResult::Line(l)) => l,
            Ok(LineResult::Eof) => break,
            Err(e) => return Err(e.into()),
        };
        let input = line.trim().to_string();
        if input.is_empty() {
            continue;
        }
        if history.last().map(|h| h != &input).unwrap_or(true) {
            history.push(input.clone());
        }

        // Chat-local commands.
        match input.as_str() {
            "/exit" | "/quit" | "/q" | "exit" | "quit" => break,
            "/help" | "/h" => {
                print_help();
                continue;
            }
            "/trace" => {
                print_trace(&last_trace);
                continue;
            }
            _ => {}
        }

        // Any other slash command → the shared management dispatcher (wizards,
        // pickers, list/get/run/etc.).
        if input.starts_with('/') {
            match dispatch(&input, config).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => println!("  {} {}", "✗".truecolor(0xE0, 0x5A, 0x4A), e),
            }
            continue;
        }

        // Plain prose → talk to MIND, with the recent transcript for context.
        let before = pending_ids(config).await;
        let mind_input = build_mind_input(&convo, &input);
        match run_mind_streaming(config, &mind_input).await {
            Ok((reply, tools)) => {
                // The trace was already printed live during the run; just keep it
                // for `/trace`.
                last_trace = tools;
                let reply = reply.trim().to_string();
                println!();
                typewriter(&reply).await;
                convo.push((input.clone(), reply));
                // Keep the last ~10 turns of context.
                if convo.len() > 10 {
                    convo.drain(0..convo.len() - 10);
                }
            }
            Err(e) => println!("\n{} {}", "✗".truecolor(0xE0, 0x5A, 0x4A), e),
        }
        offer_new_proposals(config, &before).await;
        println!();
    }

    save_history(&history);
    println!(
        "{}",
        "  Until next time — your agents keep running in the background.".dimmed()
    );
    Ok(())
}

// ── MIND run + streaming ────────────────────────────────────────────────────

/// Kick off a MIND run, animate the `a*` spinner while it works, and return its
/// final output plus the `(tool_name, result)` calls it made this turn. (A single
/// run has no live reasoning trace to tail — the daemon log is just the execution
/// record — so the trace is shown after completion and the reply typewrites.)
async fn run_mind_streaming(
    config: &AppConfig,
    input: &str,
) -> Result<(String, Vec<(String, String)>)> {
    let execution_id = match daemon_request(
        config,
        DaemonRequest::RunAgent {
            id: "MIND".to_string(),
            input: Some(input.to_string()),
        },
    )
    .await?
    {
        DaemonResponse::ExecutionStarted { execution_id } => execution_id,
        DaemonResponse::Error { message } => return Err(anyhow!(message)),
        _ => return Err(anyhow!("Unexpected response from daemon")),
    };

    let started = std::time::Instant::now();
    let timeout = Duration::from_secs(15 * 60);
    let mut frame = 0usize;
    // How many tool-call lines we've already printed above the spinner. The daemon
    // checkpoints the execution after each tool call, so this grows live and turns
    // the old 10-minute silence into a running "↳ called X" trace.
    let mut printed_tools = 0usize;

    loop {
        if started.elapsed() > timeout {
            clear_status_line();
            return Err(anyhow!("MIND timed out. Check: agenta logs MIND"));
        }

        // Spinner + live elapsed, so it's always visibly making progress.
        print!(
            "\r\x1b[2K  {}  {} {}",
            FRAMES[frame % FRAMES.len()]
                .truecolor(ORANGE.0, ORANGE.1, ORANGE.2)
                .bold(),
            "thinking".dimmed(),
            format!("· {}", fmt_elapsed(started.elapsed())).dimmed(),
        );
        std::io::stdout().flush().ok();
        frame += 1;

        match daemon_request(
            config,
            DaemonRequest::GetExecution {
                id: execution_id.to_string(),
            },
        )
        .await?
        {
            DaemonResponse::ExecutionResult { result } => {
                let tools = extract_tool_calls(&result);

                // Print any tool calls that landed since the last poll, above the
                // spinner, so the user sees what MIND is doing as it happens.
                if tools.len() > printed_tools {
                    clear_status_line();
                    for (name, _) in &tools[printed_tools..] {
                        println!("  {} {}", "↳".dimmed(), format!("called {name}").dimmed());
                    }
                    printed_tools = tools.len();
                }

                let status = result
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                let completed = result.get("completed_at").map(|v| !v.is_null()).unwrap_or(false);

                if completed
                    || status.contains("completed")
                    || status.contains("failed")
                    || status.contains("cancelled")
                {
                    clear_status_line();
                    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
                        if !err.trim().is_empty() {
                            return Err(anyhow!(err.trim().to_string()));
                        }
                    }
                    // A quiet footer with the total, so "how long did that take?" is
                    // always answerable without a stopwatch.
                    println!("  {}", format!("done in {}", fmt_elapsed(started.elapsed())).dimmed());
                    let output = result.get("output").and_then(|v| v.as_str()).unwrap_or("");
                    return Ok((clean_output(output), tools));
                }
            }
            DaemonResponse::Error { message } => {
                if !message.to_lowercase().contains("not found") {
                    clear_status_line();
                    return Err(anyhow!(message));
                }
            }
            _ => {
                clear_status_line();
                return Err(anyhow!("Unexpected response from daemon"));
            }
        }

        tokio::time::sleep(Duration::from_millis(140)).await;
    }
}

/// Compact elapsed time for the spinner and footer: "0:07", "1:42", "12:05".
fn fmt_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Reveal the reply with a typewriter effect under a left border, so it reads as
/// MIND's turn and "streams" out. The text is hard-wrapped to the terminal width
/// first so every visual line gets the border (soft-wrapped lines otherwise
/// wouldn't). Per-char delay shrinks for long replies.
async fn typewriter(text: &str) {
    let gutter = format!("{} ", "│".truecolor(ORANGE.0, ORANGE.1, ORANGE.2));
    // Reserve 2 cols for the "│ " gutter.
    let wrapped = wrap_text(text, term_width().saturating_sub(2));
    let chars: Vec<char> = wrapped.chars().collect();
    let per_char = match chars.len() {
        0..=300 => Duration::from_micros(4500),
        301..=800 => Duration::from_micros(1800),
        _ => Duration::from_micros(600),
    };

    let mut out = std::io::stdout();
    let _ = write!(out, "{gutter}");
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' {
            let _ = write!(out, "\n{gutter}");
        } else {
            let _ = write!(out, "{c}");
        }
        if i % 3 == 0 {
            out.flush().ok();
        }
        tokio::time::sleep(per_char).await;
    }
    let _ = writeln!(out);
    out.flush().ok();
}

// ── Inline proposal approval ────────────────────────────────────────────────

async fn pending_ids(config: &AppConfig) -> HashSet<String> {
    fetch_pending(config).await.into_iter().map(|p| p.id).collect()
}

async fn fetch_pending(config: &AppConfig) -> Vec<Proposal> {
    match daemon_request(
        config,
        DaemonRequest::ListProposals {
            status: Some("pending".to_string()),
        },
    )
    .await
    {
        Ok(DaemonResponse::ProposalList { proposals }) => proposals
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// For each proposal MIND created this turn, offer to approve it right here.
/// The terminal is in normal (cooked) mode between turns, so a plain stdin read
/// is enough for the y/N/view prompt.
async fn offer_new_proposals(config: &AppConfig, before: &HashSet<String>) {
    let fresh: Vec<Proposal> = fetch_pending(config)
        .await
        .into_iter()
        .filter(|p| !before.contains(&p.id))
        .collect();

    let mut resolved_any = false;
    for p in fresh {
        let short: String = p.id.chars().take(8).collect();
        println!(
            "\n  {} {} · risk {}",
            "◆".truecolor(ORANGE.0, ORANGE.1, ORANGE.2),
            p.summary().bold(),
            risk_label(p.risk),
        );
        loop {
            print!("  approve {short}? [{}/N/view] ", "y".green());
            std::io::stdout().flush().ok();
            let mut answer = String::new();
            if std::io::stdin().read_line(&mut answer).is_err() {
                // EOF — leave it pending. Still refresh the count if we already
                // resolved something earlier in this batch.
                if resolved_any {
                    print_pending_count(config).await;
                }
                return;
            }
            match answer.trim().to_lowercase().as_str() {
                "y" | "yes" => {
                    approve(config, &p.id).await;
                    resolved_any = true;
                    break;
                }
                "v" | "view" => {
                    show_payload(&p);
                    continue;
                }
                _ => {
                    println!(
                        "  {} left pending — {}",
                        "·".dimmed(),
                        format!("agenta approve {short}").dimmed()
                    );
                    break;
                }
            }
        }
    }

    // The startup welcome box shows a one-time pending count. If we resolved any
    // proposal this turn, print a refreshed live count so that snapshot doesn't
    // go stale on screen.
    if resolved_any {
        print_pending_count(config).await;
    }
}

/// Print the current live pending-proposal count — a small refresher line so the
/// startup welcome box's snapshot count can't mislead after in-chat approvals.
async fn print_pending_count(config: &AppConfig) {
    let n = fetch_pending(config).await.len();
    let msg = match n {
        0 => "no proposals pending".to_string(),
        1 => "1 proposal pending".to_string(),
        _ => format!("{n} proposals pending"),
    };
    println!("  {} {}", "⌁".truecolor(ORANGE.0, ORANGE.1, ORANGE.2), msg.dimmed());
}

async fn approve(config: &AppConfig, id: &str) {
    match daemon_request(config, DaemonRequest::ApproveProposal { id: id.to_string() }).await {
        Ok(DaemonResponse::ProposalDetails { proposal }) => {
            if let Ok(p) = serde_json::from_value::<Proposal>(proposal) {
                match p.status {
                    ProposalStatus::Applied => {
                        println!("  {} {}", "✓".green(), p.result.as_deref().unwrap_or("applied"))
                    }
                    ProposalStatus::Failed => {
                        println!("  {} {}", "✗".red(), p.result.as_deref().unwrap_or("apply failed"))
                    }
                    _ => {}
                }
            }
        }
        Ok(DaemonResponse::Error { message }) => println!("  {} {}", "✗".red(), message),
        _ => {}
    }
}

fn show_payload(p: &Proposal) {
    println!("  {}", "payload (applied on approval):".dimmed());
    if let Ok(s) = serde_json::to_string_pretty(&p.action) {
        for line in s.lines() {
            println!("    {}", line.dimmed());
        }
    }
}

fn risk_label(risk: Risk) -> String {
    match risk {
        Risk::Low => "low".green().to_string(),
        Risk::Elevated => "elevated".yellow().to_string(),
        Risk::Destructive => "DESTRUCTIVE".red().bold().to_string(),
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn clear_status_line() {
    print!("\r\x1b[2K");
    std::io::stdout().flush().ok();
}

/// Build MIND's input for this turn: the recent transcript (so it has memory of
/// the conversation) plus the new message. A single execution is otherwise
/// stateless.
fn build_mind_input(convo: &[(String, String)], new_msg: &str) -> String {
    if convo.is_empty() {
        return new_msg.to_string();
    }
    let mut s = String::from("[Conversation so far — for context]\n");
    for (user, mind) in convo {
        s.push_str(&format!("User: {user}\nYou (MIND): {mind}\n\n"));
    }
    s.push_str(&format!("[The user's new message — respond to this]\n{new_msg}"));
    s
}

/// Pull the `(tool_name, result)` calls out of a completed execution record.
fn extract_tool_calls(result: &serde_json::Value) -> Vec<(String, String)> {
    result
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let name = tc.get("tool_name").and_then(|v| v.as_str())?.to_string();
                    let res = tc
                        .get("result")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((name, res))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Show the last turn's tool calls with their results — the "closed" chain of
/// thought, opened on demand via `/trace`.
fn print_trace(trace: &[(String, String)]) {
    if trace.is_empty() {
        println!("  {}", "No tool calls in the last turn.".dimmed());
        return;
    }
    let width = term_width().saturating_sub(4);
    for (name, result) in trace {
        println!("  {} {}", "↳".truecolor(ORANGE.0, ORANGE.1, ORANGE.2), name.bold());
        let shown: String = result.trim().chars().take(1_000).collect();
        for line in wrap_text(&shown, width).lines() {
            println!("    {}", line.dimmed());
        }
    }
}

/// Current terminal width in columns (sane fallback + clamp).
fn term_width() -> usize {
    crossterm::terminal::size()
        .map(|(cols, _)| cols as usize)
        .unwrap_or(80)
        .clamp(40, 200)
}

/// Word-wrap each line to `width`, preserving blank lines and leading indent so
/// the reply's paragraphs and lists stay readable under the border.
fn wrap_text(text: &str, width: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let indent_len = line.len() - line.trim_start().len();
        let indent = &line[..indent_len];
        let avail = width.saturating_sub(indent.chars().count()).max(8);

        let mut cur = String::new();
        let mut cur_len = 0usize;
        for word in line[indent_len..].split_whitespace() {
            let wlen = word.chars().count();
            if cur_len == 0 {
                cur.push_str(word);
                cur_len = wlen;
            } else if cur_len + 1 + wlen <= avail {
                cur.push(' ');
                cur.push_str(word);
                cur_len += 1 + wlen;
            } else {
                out.push(format!("{indent}{cur}"));
                cur = word.to_string();
                cur_len = wlen;
            }
        }
        out.push(format!("{indent}{cur}"));
    }
    out.join("\n")
}

/// Drop a leading `TASK_COMPLETE:` marker if the model left one in the output.
fn clean_output(raw: &str) -> String {
    raw.replacen("TASK_COMPLETE:", "", 1).trim().to_string()
}

/// Where to persist chat history (`~/.agenta/chat_history`).
fn history_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".agenta").join("chat_history"))
}

fn load_history() -> Vec<String> {
    history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn save_history(history: &[String]) {
    if let Some(p) = history_path() {
        // Keep the last 500 entries.
        let start = history.len().saturating_sub(500);
        let _ = std::fs::write(p, history[start..].join("\n"));
    }
}

// ── Welcome box ─────────────────────────────────────────────────────────────

/// The agenta lettermark shown in the box.
const LOGO: &[&str] = &["a*"];

#[derive(Clone, Copy)]
enum Sty {
    Plain,
    Dim,
    Accent,
    AccentBold,
    Green,
    Yellow,
}

/// Startup facts for the welcome box.
struct Welcome {
    version: String,
    running: bool,
    pid: Option<u32>,
    agents: usize,
    tools: usize,
    kbs: usize,
    pending: usize,
    recent: Option<String>,
    mind_model: String,
    mind_provider: String,
    cwd: String,
}

async fn print_welcome(config: &AppConfig) {
    let w = gather_welcome(config).await;
    render_welcome(&w);
}

async fn gather_welcome(config: &AppConfig) -> Welcome {
    let (running, pid, version) = match daemon_request(config, DaemonRequest::Ping).await {
        Ok(DaemonResponse::Status { running, pid, version }) => (running, pid, version),
        _ => (false, None, env!("CARGO_PKG_VERSION").to_string()),
    };

    let agents_json = match daemon_request(config, DaemonRequest::ListAgents).await {
        Ok(DaemonResponse::AgentList { agents }) => agents,
        _ => Vec::new(),
    };
    let agents = agents_json.len();

    // Knowledge bases in use across the worker agents.
    let mut kb_set: HashSet<String> = HashSet::new();
    for a in &agents_json {
        if let Some(kbs) = a["config"]["knowledge_bases"].as_array() {
            for k in kbs {
                if let Some(s) = k.as_str() {
                    kb_set.insert(s.to_string());
                }
            }
        }
    }

    // MIND is a system agent (excluded from ListAgents), so fetch it directly.
    let (mind_model, mind_provider) =
        match daemon_request(config, DaemonRequest::GetAgent { id: "MIND".to_string() }).await {
            Ok(DaemonResponse::AgentDetails { agent }) => (
                agent["model"].as_str().unwrap_or("unknown").to_string(),
                agent["provider"].as_str().unwrap_or("").to_string(),
            ),
            _ => ("unknown".to_string(), String::new()),
        };

    let tools = match daemon_request(config, DaemonRequest::ListTools).await {
        Ok(DaemonResponse::ToolList { tools }) => tools.len(),
        _ => 0,
    };

    let pending_props = fetch_pending(config).await;
    let pending = pending_props.len();
    let recent = pending_props.first().map(|p| p.summary());

    Welcome {
        version,
        running,
        pid,
        agents,
        tools,
        kbs: kb_set.len(),
        pending,
        recent,
        mind_model,
        mind_provider,
        cwd: short_cwd(),
    }
}

fn render_welcome(w: &Welcome) {
    let total = term_width().clamp(64, 100);
    let lw = 30usize;
    let rw = total.saturating_sub(37).max(24);

    // Left column: identity + health.
    let mut left: Vec<(String, Sty)> = Vec::new();
    left.push((String::new(), Sty::Plain));
    for l in LOGO {
        left.push((center(l, lw), Sty::AccentBold));
    }
    left.push((String::new(), Sty::Plain));
    left.push((center("talk to MIND", lw), Sty::AccentBold));
    left.push((String::new(), Sty::Plain));
    left.push((center(&format!("MIND · {}", w.mind_model), lw), Sty::Plain));
    if !w.mind_provider.is_empty() {
        left.push((center(&format!("via {}", w.mind_provider), lw), Sty::Dim));
    }
    let daemon = if w.running {
        match w.pid {
            Some(p) => format!("● running · pid {}", p),
            None => "● running".to_string(),
        }
    } else {
        "● daemon down".to_string()
    };
    left.push((center(&daemon, lw), if w.running { Sty::Green } else { Sty::Yellow }));
    left.push((center(&fit_end(&w.cwd, lw), lw), Sty::Dim));

    // Right column: tips + what needs you.
    let mut right: Vec<(String, Sty)> = Vec::new();
    right.push(("Getting started".to_string(), Sty::AccentBold));
    let tip = "Build tools & agents, craft prompts, or ask anything. MIND drafts, you approve — nothing changes until you say go.";
    for line in wrap_text(tip, rw).lines() {
        right.push((line.to_string(), Sty::Dim));
    }
    right.push(("/help · /trace · agenta dashboard".to_string(), Sty::Accent));
    right.push(("─".repeat(rw), Sty::Dim));
    right.push(("Ecosystem".to_string(), Sty::AccentBold));
    right.push((
        format!("{} agents · {} tools · {} KB", w.agents, w.tools, w.kbs),
        Sty::Plain,
    ));
    if w.pending > 0 {
        let plural = if w.pending == 1 { "" } else { "s" };
        right.push((
            format!("⚠ {} proposal{} awaiting review", w.pending, plural),
            Sty::Yellow,
        ));
        if let Some(r) = &w.recent {
            right.push((format!("  latest: {}", r), Sty::Dim));
        }
    } else {
        right.push(("no proposals pending".to_string(), Sty::Dim));
    }

    // Draw.
    let accent = |s: &str| s.truecolor(ORANGE.0, ORANGE.1, ORANGE.2).to_string();
    let bar = accent("│");

    // Top border with the version title embedded in the left segment.
    let mut top_left = format!("─ agenta v{} ", w.version);
    let cur = top_left.chars().count();
    if cur < lw + 2 {
        top_left.push_str(&"─".repeat(lw + 2 - cur));
    } else {
        top_left = top_left.chars().take(lw + 2).collect();
    }
    println!();
    println!(
        "{}",
        accent(&format!("╭{}┬{}╮", top_left, "─".repeat(rw + 2)))
    );

    let rows = left.len().max(right.len());
    for i in 0..rows {
        let (lt, ls) = left.get(i).cloned().unwrap_or((String::new(), Sty::Plain));
        let (rt, rs) = right.get(i).cloned().unwrap_or((String::new(), Sty::Plain));
        println!("{bar} {} {bar} {} {bar}", paint(&lt, lw, ls), paint(&rt, rw, rs));
    }

    println!(
        "{}",
        accent(&format!("╰{}┴{}╯", "─".repeat(lw + 2), "─".repeat(rw + 2)))
    );
    println!();
}

/// Pad `text` to `w` display columns (truncating if longer) then apply style.
fn paint(text: &str, w: usize, s: Sty) -> String {
    let mut t: String = text.chars().take(w).collect();
    let len = t.chars().count();
    if len < w {
        t.push_str(&" ".repeat(w - len));
    }
    match s {
        Sty::Plain => t,
        Sty::Dim => t.dimmed().to_string(),
        Sty::Accent => t.truecolor(ORANGE.0, ORANGE.1, ORANGE.2).to_string(),
        Sty::AccentBold => t.truecolor(ORANGE.0, ORANGE.1, ORANGE.2).bold().to_string(),
        Sty::Green => t.truecolor(0x7C, 0xE3, 0x8B).to_string(),
        Sty::Yellow => t.truecolor(0xE8, 0xC2, 0x5A).to_string(),
    }
}

/// Center `text` within `w` columns.
fn center(text: &str, w: usize) -> String {
    let t: String = text.chars().take(w).collect();
    let len = t.chars().count();
    let pad = w - len;
    let l = pad / 2;
    format!("{}{}{}", " ".repeat(l), t, " ".repeat(pad - l))
}

/// Truncate keeping the END (for paths): "…/tail".
fn fit_end(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        s.to_string()
    } else {
        let tail: String = s.chars().skip(n - (w - 1)).collect();
        format!("…{}", tail)
    }
}

fn short_cwd() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = cwd.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    cwd.display().to_string()
}

fn print_help() {
    println!();
    println!("  {}", "Chatting with MIND".bold());
    println!("    Just type a request, e.g. \"create an HTTP tool for weather\".");
    println!("    MIND drafts it as a proposal; approve it right here, or later via the CLI.");
    println!();
    println!("  {}", "Proposals".bold());
    println!("    After MIND proposes, answer {}/{}/{} inline.", "y".green(), "N".bold(), "view".dimmed());
    println!(
        "    Or manage them anytime: {}",
        "agenta proposals · approve <id> · reject <id>".truecolor(ORANGE.0, ORANGE.1, ORANGE.2)
    );
    println!();
    println!("  {}", "Session".bold());
    println!(
        "    {} show the last turn's tool calls + results (the chain of thought)",
        "/trace".truecolor(ORANGE.0, ORANGE.1, ORANGE.2)
    );
    println!(
        "    Type {} to open the command menu (↑/↓ pick · Enter run) — create/list/get agents & tools, KBs, run, logs, status.",
        "/".truecolor(ORANGE.0, ORANGE.1, ORANGE.2)
    );
    println!("    ↑/↓ history · /trace last tool calls · /exit leave (agents keep running)");
    println!();
}
