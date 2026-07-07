use super::{Commands, ProposalCommands, PullCommands, ScriptCommands, SetupCommands, ToolCommands, ViewCommands};
use crate::core::{
    Agent, AgentStatus, AppConfig, DaemonRequest, DaemonResponse, DeepAgentConfig, ExecutionMode,
    ExecutionResult, HttpHandler, Proposal, ProposalStatus, ScriptDefinition, SideEffect,
    ToolDefinition, ToolResource,
};
use anyhow::{anyhow, Context, Result};
use comfy_table::{Cell, CellAlignment, Table};
use owo_colors::OwoColorize;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Send a request to the daemon and return the response
pub(crate) async fn daemon_request(config: &AppConfig, request: DaemonRequest) -> Result<DaemonResponse> {
    let socket_path = Path::new(&config.socket_path);

    if !socket_path.exists() {
        return Err(anyhow!(
            "Daemon is not running. Start it with: agenta daemon start"
        ));
    }

    let mut stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| "Failed to connect to daemon")?;

    let request_bytes = serde_json::to_vec(&request)?;
    stream.write_all(&request_bytes).await?;
    stream.shutdown().await?;

    let mut buffer = Vec::with_capacity(16 * 1024);
    stream.read_to_end(&mut buffer).await?;

    let response: DaemonResponse = serde_json::from_slice(&buffer)?;
    Ok(response)
}

async fn is_daemon_running(config: &AppConfig) -> bool {
    matches!(
        daemon_request(config, DaemonRequest::Ping).await,
        Ok(DaemonResponse::Status { running: true, .. })
    )
}

pub async fn handle_command(command: Commands, config: AppConfig) -> Result<()> {
    match command {
        Commands::Shell => {
            super::shell::run_shell(config).await?;
            return Ok(());
        },

        Commands::Create {
            name,
            model,
            prompt,
            prompt_file,
            description,
            temperature,
            top_p,
            max_tokens,
            mode,
            schedule,
            deep,
            deep_iterations,
            memory,
            provider,
            tools,
            allow_destructive_tools,
            interactive,
        } => {
            if interactive {
                return create_interactive(config).await;
            }

            let system_prompt = if let Some(file) = prompt_file {
                std::fs::read_to_string(file)?
            } else {
                prompt.unwrap_or_else(|| "You are a helpful AI assistant.".to_string())
            };

            let mut agent = Agent::new(name, model, system_prompt);
            agent.description = description;
            agent.config.temperature = temperature;
            agent.config.top_p = top_p;
            agent.config.max_tokens = max_tokens;
            agent.execution_mode = parse_execution_mode(&mode)?;
            agent.schedule = schedule;
            agent.memory_enabled = memory;
            agent.provider = provider;
            agent.config.allow_destructive_tools = allow_destructive_tools;
            if let Some(tools_arg) = tools {
                agent.tools = read_tool_definitions(&tools_arg)?;
            }

            if deep {
                agent.deep_agent_config = Some(DeepAgentConfig {
                    max_iterations: deep_iterations,
                    enable_reflection: true,
                    available_tools: agent.tools.iter().map(|t| t.name.clone()).collect(),
                    stop_conditions: vec!["task_complete".to_string()],
                    allow_sub_agents: false,
                    subagent_spawn_message: None,
                });
            }

            let request = DaemonRequest::CreateAgent {
                agent: serde_json::to_value(agent)?,
            };

            match daemon_request(&config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        Commands::Get { id, full: _ } => {
            let request = DaemonRequest::GetAgent { id };

            match daemon_request(&config, request).await? {
                DaemonResponse::AgentDetails { agent } => {
                    let agent: Agent = serde_json::from_value(agent)?;
                    print_agent_details(&agent);
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        Commands::List { status, all: _ } => {
            let request = DaemonRequest::ListAgents;

            match daemon_request(&config, request).await? {
                DaemonResponse::AgentList { agents } => {
                    let agents: Vec<Agent> = agents
                        .into_iter()
                        .filter_map(|v| serde_json::from_value(v).ok())
                        .collect();
                    print_agents_table(&agents, status.as_deref());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        Commands::Update {
            id,
            name: new_name,
            model: new_model,
            prompt: new_prompt,
            prompt_file: new_prompt_file,
            description: new_description,
            temperature: new_temp,
            max_tokens: new_max_tokens,
            mode: new_mode,
            schedule: new_schedule,
            scheduled_input: new_scheduled_input,
            memory: new_memory,
            provider: new_provider,
            tools: new_tools,
            deep: new_deep,
            deep_iterations: new_deep_iterations,
            add_tool: new_add_tool,
            remove_tool: new_remove_tool,
            add_kb: new_add_kb,
            remove_kb: new_remove_kb,
            top_k: new_top_k,
            allow_destructive_tools: new_allow_destructive,
            spawn_message: new_spawn_message,
        } => {
            let get_request = DaemonRequest::GetAgent { id: id.clone() };
            let agent_response = daemon_request(&config, get_request).await?;

            let mut agent: Agent = match agent_response {
                DaemonResponse::AgentDetails { agent } => {
                    serde_json::from_value(agent)?
                }
                DaemonResponse::Error { message } => return Err(anyhow!("{}", message)),
                _ => return Err(anyhow!("Unexpected response")),
            };

            if let Some(name) = new_name {
                agent.name = name;
            }
            if let Some(model) = new_model {
                agent.model = model;
            }
            if let Some(file) = new_prompt_file {
                agent.system_prompt = std::fs::read_to_string(&file)
                    .with_context(|| format!("reading --prompt-file {}", file))?;
            }
            if let Some(prompt) = new_prompt {
                agent.system_prompt = prompt;
            }
            if let Some(desc) = new_description {
                agent.description = Some(desc);
            }
            if let Some(temp) = new_temp {
                agent.config.temperature = temp;
            }
            if let Some(max_tokens) = new_max_tokens {
                agent.config.max_tokens = max_tokens;
            }
            if let Some(mode) = new_mode {
                agent.execution_mode = parse_execution_mode(&mode)?;
            }
            if new_schedule.is_some() {
                agent.schedule = new_schedule;
            }
            if new_scheduled_input.is_some() {
                // Empty string clears the directive (back to no scheduled input).
                agent.scheduled_input = new_scheduled_input.filter(|s| !s.trim().is_empty());
            }
            if let Some(mem) = new_memory {
                agent.memory_enabled = mem;
            }
            if new_provider.is_some() {
                agent.provider = new_provider;
            }
            if let Some(tools_arg) = new_tools {
                agent.tools = read_tool_definitions(&tools_arg)?;
                if let Some(config) = agent.deep_agent_config.as_mut() {
                    config.available_tools = agent.tools.iter().map(|t| t.name.clone()).collect();
                }
            }
            if let Some(tool_name) = new_add_tool {
                // Resolve the tool: prefer the DB registry (where `agenta tool
                // create` and MIND's approved proposals live), and fall back to a
                // filesystem manifest for legacy `agenta pull` tools.
                let tool: ToolDefinition = match daemon_request(
                    &config,
                    DaemonRequest::GetTool { id: tool_name.clone() },
                )
                .await
                {
                    Ok(DaemonResponse::ToolDetails { tool }) => {
                        serde_json::from_value::<ToolResource>(tool)?.as_definition()
                    }
                    _ => read_installed_tool(&tool_name)?,
                };
                // Replace if already present, otherwise append
                if let Some(pos) = agent.tools.iter().position(|t| t.name == tool.name) {
                    agent.tools[pos] = tool;
                } else {
                    agent.tools.push(tool);
                }
                if let Some(cfg) = agent.deep_agent_config.as_mut() {
                    cfg.available_tools = agent.tools.iter().map(|t| t.name.clone()).collect();
                }
            }
            if let Some(tool_name) = new_remove_tool {
                let before = agent.tools.len();
                agent.tools.retain(|t| t.name != tool_name);
                if agent.tools.len() == before {
                    println!("{} Tool '{}' was not attached to this agent.", "!".yellow(), tool_name);
                }
                if let Some(cfg) = agent.deep_agent_config.as_mut() {
                    cfg.available_tools = agent.tools.iter().map(|t| t.name.clone()).collect();
                }
            }
            if let Some(kb) = new_add_kb {
                if !agent.config.knowledge_bases.contains(&kb) {
                    agent.config.knowledge_bases.push(kb);
                }
            }
            if let Some(kb) = new_remove_kb {
                let before = agent.config.knowledge_bases.len();
                agent.config.knowledge_bases.retain(|k| k != &kb);
                if agent.config.knowledge_bases.len() == before {
                    println!("{} Knowledge base '{}' was not attached to this agent.", "!".yellow(), kb);
                }
            }
            if let Some(k) = new_top_k {
                agent.config.rag_top_k = Some(k);
            }
            if let Some(v) = new_allow_destructive {
                agent.config.allow_destructive_tools = v;
            }
            match new_deep {
                Some(true) => {
                    // Enable deep mode, preserving an existing config's fields where possible.
                    let tool_names: Vec<String> = agent.tools.iter().map(|t| t.name.clone()).collect();
                    agent.deep_agent_config = Some(match agent.deep_agent_config.take() {
                        Some(mut cfg) => {
                            cfg.max_iterations = new_deep_iterations;
                            cfg.available_tools = tool_names;
                            cfg
                        }
                        None => DeepAgentConfig {
                            max_iterations: new_deep_iterations,
                            enable_reflection: true,
                            available_tools: tool_names,
                            stop_conditions: vec!["task_complete".to_string()],
                            allow_sub_agents: false,
                            subagent_spawn_message: None,
                        },
                    });
                }
                Some(false) => agent.deep_agent_config = None,
                None => {}
            }
            if let Some(msg) = new_spawn_message {
                if let Some(config) = agent.deep_agent_config.as_mut() {
                    config.subagent_spawn_message = if msg.is_empty() { None } else { Some(msg) };
                }
            }

            agent.touch();

            let request = DaemonRequest::UpdateAgent {
                id: agent.id.clone(),
                agent: serde_json::to_value(agent)?,
            };

            match daemon_request(&config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        Commands::Delete { id, force } => {
            if !force {
                print!("Are you sure you want to delete agent {}? [y/N] ", id);
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
            }

            let request = DaemonRequest::DeleteAgent { id };

            match daemon_request(&config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        Commands::Run {
            id,
            input,
            input_file,
            wait,
            follow,
        } => {
            let input_text = if let Some(file) = input_file {
                std::fs::read_to_string(file)?
            } else {
                input.unwrap_or_default()
            };

            let request = DaemonRequest::RunAgent {
                id,
                input: if input_text.is_empty() {
                    None
                } else {
                    Some(input_text)
                },
            };

            match daemon_request(&config, request).await? {
                DaemonResponse::ExecutionStarted { execution_id } => {
                    println!("Agent execution started: {}", execution_id.blue());
                    if wait || follow {
                        wait_for_execution(&config, &execution_id).await?;
                    }
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        Commands::Stop { id } => {
            let request = DaemonRequest::StopAgent { id };

            match daemon_request(&config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        Commands::Logs {
            agent_id,
            execution_id,
            lines,
            follow,
        } => {
            let request = DaemonRequest::GetLogs {
                agent_id,
                execution_id,
                lines,
            };

            if follow {
                follow_logs(&config, request).await
            } else {
                match daemon_request(&config, request).await? {
                    DaemonResponse::ExecutionLog { lines } => {
                        for line in lines {
                            println!("{}", line);
                        }
                        Ok(())
                    }
                    DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                    _ => Err(anyhow!("Unexpected response")),
                }
            }
        }

        Commands::Setup { target } => match target {
            None                          => bootstrap_mind(&config).await,
            Some(SetupCommands::Telegram) => setup_telegram(&config).await,
        },

        Commands::Pull { target } => match target {
            PullCommands::Tool { name, version, attach } => {
                pull_tool(&config, &name, &version, attach.as_deref()).await
            }
        },

        Commands::Knowledge { command } => {
            super::knowledge::handle_knowledge_command(command, &config).await
        }

        Commands::Daemon { command } => handle_daemon_command(command, config).await,

        Commands::Export { id, output, format } => {
            let request = if id == "all" {
                DaemonRequest::ListAgents
            } else {
                DaemonRequest::GetAgent { id }
            };

            match daemon_request(&config, request).await? {
                DaemonResponse::AgentList { agents } => {
                    let data = serde_json::json!({ "agents": agents });
                    write_export(&output, &data, &format)?;
                    println!("Exported {} agents to {}", agents.len(), output.green());
                    Ok(())
                }
                DaemonResponse::AgentDetails { agent } => {
                    write_export(&output, &agent, &format)?;
                    println!("Exported agent to {}", output.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        Commands::Import { input, format: _, force } => {
            let content = std::fs::read_to_string(&input)?;
            let data: serde_json::Value = serde_json::from_str(&content)?;

            let agents_to_import: Vec<Agent> = if let Some(arr) = data.get("agents").and_then(|v| v.as_array()) {
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            } else if let Ok(agent) = serde_json::from_value::<Agent>(data.clone()) {
                vec![agent]
            } else {
                return Err(anyhow!("Invalid import format"));
            };

            let total = agents_to_import.len();
            let mut created = 0usize;
            let mut updated = 0usize;
            let mut skipped = 0usize;

            for agent in agents_to_import {
                let agent_name = agent.name.clone();
                let agent_id = agent.id.clone();
                let agent_value = serde_json::to_value(&agent)?;

                // Try create first
                let create_req = DaemonRequest::CreateAgent { agent: agent_value.clone() };
                match daemon_request(&config, create_req).await? {
                    DaemonResponse::Success { .. } => {
                        println!("{} {}", "Created:".green(), agent_name);
                        created += 1;
                    }
                    DaemonResponse::Error { message } if message.contains("duplicate") || message.contains("unique") || message.contains("already") => {
                        // Agent exists — update if --force, skip otherwise
                        if force {
                            // Fetch existing agent to get its ID, then update with imported data
                            let get_req = DaemonRequest::GetAgent { id: agent_name.clone() };
                            if let Ok(DaemonResponse::AgentDetails { agent: existing }) = daemon_request(&config, get_req).await {
                                let existing_id = existing.get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&agent_id)
                                    .to_string();
                                let mut merged = agent_value.clone();
                                merged["id"] = serde_json::Value::String(existing_id.clone());
                                let update_req = DaemonRequest::UpdateAgent {
                                    id: existing_id,
                                    agent: merged,
                                };
                                match daemon_request(&config, update_req).await? {
                                    DaemonResponse::Success { .. } => {
                                        println!("{} {}", "Updated:".yellow(), agent_name);
                                        updated += 1;
                                    }
                                    DaemonResponse::Error { message } => {
                                        eprintln!("{} {}: {}", "Failed:".red(), agent_name, message);
                                        skipped += 1;
                                    }
                                    _ => {}
                                }
                            }
                        } else {
                            println!("{} {} (use --force to overwrite)", "Skipped:".yellow(), agent_name);
                            skipped += 1;
                        }
                    }
                    DaemonResponse::Error { message } => {
                        eprintln!("{} {}: {}", "Failed:".red(), agent_name, message);
                        skipped += 1;
                    }
                    _ => {}
                }
            }

            println!("\nImport complete: {} total — {} created, {} updated, {} skipped",
                total, created, updated, skipped);
            Ok(())
        }

        Commands::Completion { shell: _ } => {
            // TODO: Generate shell completions
            println!("Shell completion generation not yet implemented");
            Ok(())
        }

        Commands::Tool { command } => handle_tool_command(command, &config).await,

        Commands::Script { command } => handle_script_command(command, &config).await,

        Commands::Dashboard => crate::cli::tui::run_tui(config).await,

        Commands::Proposals { all, command } => handle_proposals_command(all, command, &config).await,

        Commands::Approve { id } => resolve_proposal(&config, &id, true, None).await,

        Commands::Reject { id, reason } => resolve_proposal(&config, &id, false, reason).await,

        Commands::View { command } => match command {
            ViewCommands::Executions { limit } => {
                let request = DaemonRequest::ListExecutions { limit };
                match daemon_request(&config, request).await? {
                    DaemonResponse::ExecutionList { executions } => {
                        let executions: Vec<ExecutionResult> = executions
                            .into_iter()
                            .filter_map(|v| serde_json::from_value(v).ok())
                            .collect();
                        print_executions_table(&executions);
                        Ok(())
                    }
                    DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
                    _ => Err(anyhow!("Unexpected response")),
                }
            }
        },

        Commands::Doctor => run_doctor(&config).await,

        Commands::Upgrade { version } => {
            println!("Upgrading agenta to {}...", version);
            let script_url = "https://raw.githubusercontent.com/warifmust/agenta/main/install.sh";
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "AGENTA_VERSION={version} curl -fsSL {url} | bash",
                    version = if version == "latest" { String::new() } else { version.clone() },
                    url = script_url
                ))
                .env("AGENTA_VERSION", &version)
                .status()
                .map_err(|e| anyhow!("Failed to run upgrade: {}", e))?;

            if status.success() {
                println!("Upgrade complete. Restart the daemon to apply:");
                println!("  agenta daemon stop && agenta daemon start");
                Ok(())
            } else {
                Err(anyhow!("Upgrade failed with exit code: {}", status))
            }
        },

        Commands::Uninstall { purge, yes } => handle_uninstall(&config, purge, yes),
    }
}

/// Remove agenta's binaries from every standard location (so a stale copy earlier
/// in PATH can't linger), and — with `--purge` — its config + local data too. Never
/// touches an external Postgres database.
fn handle_uninstall(config: &AppConfig, purge: bool, yes: bool) -> Result<()> {
    // 1. Enumerate binary locations: where we're running from + the standard dirs.
    let mut bin_dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            bin_dirs.push(d.to_path_buf());
        }
    }
    if let Some(home) = dirs::home_dir() {
        bin_dirs.push(home.join(".local/bin"));
        bin_dirs.push(home.join(".cargo/bin"));
    }
    bin_dirs.push(std::path::PathBuf::from("/usr/local/bin"));
    bin_dirs.sort();
    bin_dirs.dedup();

    let mut targets: Vec<std::path::PathBuf> = Vec::new();
    for d in &bin_dirs {
        for b in ["agenta", "agenta-daemon"] {
            let p = d.join(b);
            if p.exists() {
                targets.push(p);
            }
        }
    }

    let config_dir = dirs::home_dir().map(|h| h.join(".agenta"));
    let data_dir = std::path::Path::new(&config.database_path)
        .parent()
        .map(|p| p.to_path_buf());

    // 2. Show the plan.
    println!("{}", "This will remove:".bold());
    if targets.is_empty() {
        println!("  (no agenta binaries found in standard locations)");
    } else {
        for t in &targets {
            println!("  {}", t.display());
        }
    }
    if purge {
        if let Some(c) = &config_dir {
            println!("  {}  {}", c.display(), "(config, .env with API keys, tools)".dimmed());
        }
        if let Some(dd) = &data_dir {
            println!("  {}  {}", dd.display(), "(local database, socket)".dimmed());
        }
        if config.database_url.as_deref().map(|u| u.starts_with("postgres")).unwrap_or(false) {
            println!("{}", "Your external Postgres database is NOT touched.".yellow());
        }
    } else {
        println!("{}", "Config and data are kept. Re-run with --purge to remove them too.".dimmed());
    }

    // 3. Confirm.
    if !yes {
        let prompt = if purge {
            "Remove binaries AND all config/data? This deletes your agents and API keys. [y/N] "
        } else {
            "Remove agenta binaries? [y/N] "
        };
        print!("{}", prompt);
        std::io::stdout().flush().ok();
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !ans.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // 4. Stop the daemon so we don't leave an orphan behind.
    println!("Stopping daemon...");
    kill_all_daemons();
    cleanup_daemon_files(config);

    // 5. Remove binaries (safe to unlink our own running binary on Unix).
    let mut failed: Vec<(std::path::PathBuf, std::io::Error)> = Vec::new();
    for t in &targets {
        match std::fs::remove_file(t) {
            Ok(_) => println!("  removed {}", t.display()),
            Err(e) => failed.push((t.clone(), e)),
        }
    }

    // 6. Purge config + local data if asked.
    if purge {
        for dir in [config_dir, data_dir].into_iter().flatten() {
            if dir.exists() {
                match std::fs::remove_dir_all(&dir) {
                    Ok(_) => println!("  removed {}", dir.display()),
                    Err(e) => failed.push((dir, e)),
                }
            }
        }
    }

    // 7. Summary.
    println!();
    if failed.is_empty() {
        println!("{}", "✓ agenta uninstalled.".green());
    } else {
        println!("{}", "Uninstalled, but some items could not be removed:".yellow());
        for (p, e) in &failed {
            println!("  {} — {}", p.display(), e);
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                println!("    try: sudo rm -rf {}", p.display());
            }
        }
    }
    if !purge {
        println!("{}", "Your agents and config remain in ~/.agenta.".dimmed());
    }
    Ok(())
}

async fn run_doctor(config: &AppConfig) -> Result<()> {
    let pass  = "✓".green().to_string();
    let warn  = "⚠".yellow().to_string();
    let fail  = "✗".red().to_string();

    println!("{}", "agenta doctor — core runtime".bold());
    println!("{}", "─".repeat(44));

    // ── 1. Config file ────────────────────────────────
    let config_path = dirs::home_dir()
        .map(|h| h.join(".agenta").join("config.toml"))
        .unwrap_or_else(|| std::path::PathBuf::from("~/.agenta/config.toml"));

    if config_path.exists() {
        println!("{} Config file found ({})", pass, config_path.display());
    } else {
        println!("{} Config file not found at {} — using defaults", warn, config_path.display());
    }

    // ── 2. Daemon socket ──────────────────────────────
    let socket_path = std::path::Path::new(&config.socket_path);
    if !socket_path.exists() {
        println!("{} Daemon socket not found — run: agenta daemon start", fail);
    } else {
        // ── 3. Daemon ping ────────────────────────────
        match daemon_request(config, DaemonRequest::Ping).await {
            Ok(DaemonResponse::Status { running: true, .. }) => {
                println!("{} Daemon running and reachable", pass);
            }
            Ok(_) => {
                println!("{} Daemon socket exists but returned unexpected response", warn);
            }
            Err(e) => {
                println!("{} Daemon not responding: {}", fail, e);
            }
        }
    }

    // ── 4. Database ───────────────────────────────────
    let db_path = std::path::Path::new(&config.database_path);
    if db_path.exists() {
        println!("{} Database found ({})", pass, config.database_path);
    } else if config.database_url.is_some() {
        println!("{} Using remote database ({})", pass, config.database_url.as_deref().unwrap_or(""));
    } else {
        println!("{} Database not found at {} — daemon may not have initialised yet", warn, config.database_path);
    }

    // ── 5. Ollama ─────────────────────────────────────
    let ollama_url = format!("{}/api/tags", config.ollama_url);
    match reqwest::Client::new()
        .get(&ollama_url)
        .timeout(Duration::from_secs(3))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            println!("{} Ollama reachable at {}", pass, config.ollama_url);
        }
        Ok(resp) => {
            println!("{} Ollama responded with HTTP {} at {}", warn, resp.status(), config.ollama_url);
        }
        Err(_) => {
            println!("{} Ollama not reachable at {} — is it running?", fail, config.ollama_url);
        }
    }

    println!("{}", "─".repeat(44));
    Ok(())
}

async fn create_interactive(config: AppConfig) -> Result<()> {
    use inquire::{Confirm, Select, Text};

    let name = Text::new("Agent name:").prompt()?;
    let model = Text::new("Model:").with_default("llama2").prompt()?;
    let description = Text::new("Description:").prompt_skippable()?;

    let prompt = Text::new("System prompt:")
        .with_default("You are a helpful AI assistant.")
        .prompt()?;

    let modes = vec!["once", "scheduled", "triggered", "continuous"];
    let mode = Select::new("Execution mode:", modes).prompt()?;

    let mut agent = Agent::new(name, model, prompt);
    agent.description = description;
    agent.execution_mode = parse_execution_mode(mode)?;

    if Confirm::new("Enable deep agent mode?").with_default(false).prompt()? {
        agent.deep_agent_config = Some(DeepAgentConfig {
            max_iterations: 10,
            enable_reflection: true,
            available_tools: vec![],
            stop_conditions: vec!["task_complete".to_string()],
            allow_sub_agents: false,
            subagent_spawn_message: None,
        });
    }

    let request = DaemonRequest::CreateAgent {
        agent: serde_json::to_value(agent)?,
    };

    match daemon_request(&config, request).await? {
        DaemonResponse::Success { message } => {
            println!("{}", message.green());
            Ok(())
        }
        DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
        _ => Err(anyhow!("Unexpected response")),
    }
}

async fn handle_daemon_command(
    command: super::DaemonCommands,
    config: AppConfig,
) -> Result<()> {
    match command {
        super::DaemonCommands::Start { foreground, log_level: _ } => {
            daemon_start(&config, foreground).await
        }

        super::DaemonCommands::Stop { force: _ } => {
            daemon_stop(&config).await
        }

        super::DaemonCommands::Status => {
            let request = DaemonRequest::Ping;

            match daemon_request(&config, request).await {
                Ok(DaemonResponse::Status { running, pid, version }) => {
                    let mut table = styled_table();
                    table.set_header(vec!["Property", "Value"]);
                    table.add_row(vec!["Status", if running { "Running" } else { "Stopped" }]);
                    if let Some(pid) = pid {
                        table.add_row(vec!["PID", &pid.to_string()]);
                    }
                    table.add_row(vec!["Version", &version]);
                    println!("{}", table);
                }
                Ok(_) => println!("Daemon status: Unknown"),
                Err(_) => println!("Daemon is not running"),
            }

            Ok(())
        }

        super::DaemonCommands::Restart => {
            // daemon_stop now waits for the process to fully exit and free its
            // socket/ports, so start won't race a still-bound instance.
            daemon_stop(&config).await?;
            tokio::time::sleep(Duration::from_millis(500)).await;
            daemon_start(&config, false).await
        }

    }
}

pub async fn mind_exists(config: &AppConfig) -> bool {
    matches!(
        daemon_request(config, DaemonRequest::GetAgent { id: "MIND".into() }).await,
        Ok(DaemonResponse::AgentDetails { .. })
    )
}

/// Interactive wizard to add a Telegram bot entry to config.toml.
/// Can be called from the main setup wizard or standalone via `agenta setup telegram`.
async fn setup_telegram(config: &AppConfig) -> Result<()> {
    println!();
    println!("{}", "  Telegram Setup".bold().cyan());
    println!("  Connect a Telegram bot to an agent.");
    println!();
    println!("  1. Open Telegram and chat with @BotFather");
    println!("  2. Send /newbot and follow the prompts");
    println!("  3. Copy the token it gives you (looks like 123456:ABC...)");
    println!();

    print!("  Bot token: ");
    std::io::stdout().flush()?;
    let mut token = String::new();
    std::io::stdin().read_line(&mut token)?;
    let token = token.trim().to_string();

    if token.is_empty() {
        println!("  {} No token entered — skipping Telegram setup.", "⚠".yellow());
        return Ok(());
    }

    // List agents so user can pick
    let agents: Vec<String> = match daemon_request(config, DaemonRequest::ListAgents).await {
        Ok(DaemonResponse::AgentList { agents }) => agents
            .iter()
            .filter_map(|a: &serde_json::Value| {
                a.get("name").and_then(|n| n.as_str()).map(|s| s.to_string())
            })
            .collect(),
        _ => vec![],
    };

    let default_agent = if agents.is_empty() {
        println!("  No agents found — you can set the agent name manually.");
        print!("  Agent name: ");
        std::io::stdout().flush()?;
        let mut a = String::new();
        std::io::stdin().read_line(&mut a)?;
        a.trim().to_string()
    } else {
        println!("  Which agent should handle messages from this bot?");
        for (i, name) in agents.iter().enumerate() {
            println!("    {})  {}", (i + 1).to_string().cyan(), name.bold());
        }
        print!("  Choice [1]: ");
        std::io::stdout().flush()?;
        let mut choice = String::new();
        std::io::stdin().read_line(&mut choice)?;
        let idx: usize = choice.trim().parse().unwrap_or(1);
        agents.get(idx.saturating_sub(1)).cloned().unwrap_or_else(|| agents[0].clone())
    };

    if default_agent.is_empty() {
        println!("  {} No agent selected — skipping.", "⚠".yellow());
        return Ok(());
    }

    print!("  Friendly bot name (optional, press Enter to skip): ");
    std::io::stdout().flush()?;
    let mut label = String::new();
    std::io::stdin().read_line(&mut label)?;
    let label = label.trim().to_string();
    let bot_name = if label.is_empty() { None } else { Some(label) };

    // Read existing config.toml, append the new bot, write back
    let config_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".agenta")
        .join("config.toml");

    let toml_content = std::fs::read_to_string(&config_path).unwrap_or_default();
    let mut doc: toml::Value = toml_content.parse().unwrap_or(toml::Value::Table(toml::map::Map::new()));

    let new_bot = {
        let mut m = toml::map::Map::new();
        m.insert("token".into(), toml::Value::String(token.clone()));
        m.insert("default_agent".into(), toml::Value::String(default_agent.clone()));
        if let Some(ref n) = bot_name {
            m.insert("name".into(), toml::Value::String(n.clone()));
        }
        toml::Value::Table(m)
    };

    if let toml::Value::Table(ref mut root) = doc {
        let bots = root
            .entry("telegram_bots")
            .or_insert_with(|| toml::Value::Array(vec![]));
        if let toml::Value::Array(ref mut arr) = bots {
            arr.push(new_bot);
        }
    }

    std::fs::write(&config_path, toml::to_string_pretty(&doc)?)?;

    println!();
    println!("  {} bot added to ~/.agenta/config.toml", "✓ Telegram".green());
    println!("  Token  : {}…", &token[..token.len().min(12)]);
    println!("  Agent  : {}", default_agent.cyan());
    if let Some(ref n) = bot_name {
        println!("  Name   : {}", n);
    }
    println!();
    println!("  Restart the daemon to activate: {}", "agenta daemon restart".cyan());
    println!();

    Ok(())
}

async fn bootstrap_mind(config: &AppConfig) -> Result<()> {
    // ── Step 0: already set up? ───────────────────────────────────────────────
    let already_have_mind = matches!(
        daemon_request(config, DaemonRequest::GetAgent { id: "MIND".into() }).await,
        Ok(DaemonResponse::AgentDetails { .. })
    );
    if already_have_mind {
        println!("{}", "MIND already exists — nothing to do.".green());
        println!("Run {} to open the dashboard.", "agenta".cyan());
        return Ok(());
    }

    // ── Welcome ───────────────────────────────────────────────────────────────
    println!();
    println!("{}", "  Welcome to agenta!".bold().cyan());
    println!("  Let's get you set up. This takes about a minute.");
    println!();

    // ── Step 1: Provider ─────────────────────────────────────────────────────
    println!("{}", "  Step 1/3 — AI Provider".bold());
    println!("  Which provider do you want to use?");
    println!();
    println!("    {}  {} — local, no API key needed", "1)".cyan(), "Ollama".bold());
    println!("    {}  {} — cloud, fast & cheap", "2)".cyan(), "DeepSeek".bold());
    println!("    {}  {} — cloud, 300+ models", "3)".cyan(), "OpenRouter".bold());
    println!("    {}  {} — cloud, GPT models", "4)".cyan(), "OpenAI".bold());
    println!();
    print!("  Choice [1]: ");
    std::io::stdout().flush()?;

    let mut choice = String::new();
    std::io::stdin().read_line(&mut choice)?;
    let provider = match choice.trim() {
        "2" => "deepseek",
        "3" => "openrouter",
        "4" => "openai",
        _   => "ollama",
    };
    println!("  {} {}", "✓ Provider:".green(), provider);
    println!();

    // ── Step 2: API key (cloud providers only) ────────────────────────────────
    let api_key = if provider != "ollama" {
        let (key_url, env_var) = match provider {
            "deepseek"    => ("platform.deepseek.com/api_keys", "DEEPSEEK_API_KEY"),
            "openrouter"  => ("openrouter.ai/keys",             "OPENROUTER_API_KEY"),
            _             => ("platform.openai.com/api-keys",   "OPENAI_API_KEY"),
        };
        println!("{}", "  Step 2/3 — API Key".bold());
        println!("  Get your key at: {}", key_url.cyan());
        print!("  API key: ");
        std::io::stdout().flush()?;

        let mut key = String::new();
        std::io::stdin().read_line(&mut key)?;
        let key = key.trim().to_string();

        if !key.is_empty() {
            // Append to ~/.agenta/.env
            let env_path = dirs::home_dir()
                .unwrap_or_default()
                .join(".agenta")
                .join(".env");
            let entry = format!("{}={}\n", env_var, key);
            // Remove existing line for this var, then append
            let existing = std::fs::read_to_string(&env_path).unwrap_or_default();
            let filtered: String = existing
                .lines()
                .filter(|l| !l.starts_with(&format!("{}=", env_var)))
                .map(|l| format!("{}\n", l))
                .collect();
            std::fs::write(&env_path, format!("{}{}", filtered, entry))?;
            println!("  {} saved to ~/.agenta/.env", "✓ Key".green());
        } else {
            println!("  {} — you can add it later to ~/.agenta/.env", "⚠ Skipped".yellow());
        }
        println!();
        Some(key)
    } else {
        None
    };
    let _ = api_key; // used for env write, not needed further

    // ── Step 3: Model ────────────────────────────────────────────────────────
    let default_model = match provider {
        "deepseek"    => "deepseek-chat",
        "openrouter"  => "anthropic/claude-3.5-haiku",
        "openai"      => "gpt-4o-mini",
        _             => "qwen3:latest",
    };
    println!("{}", "  Step 3/3 — Model".bold());
    println!("  Which model? (press Enter for {})", default_model.cyan());
    print!("  Model: ");
    std::io::stdout().flush()?;

    let mut model_input = String::new();
    std::io::stdin().read_line(&mut model_input)?;
    let model = {
        let m = model_input.trim();
        if m.is_empty() { default_model.to_string() } else { m.to_string() }
    };
    println!("  {} {}", "✓ Model:".green(), model);
    println!();

    // ── Write config.toml ────────────────────────────────────────────────────
    let config_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".agenta")
        .join("config.toml");

    if !config_path.exists() {
        // Write a fresh config with the chosen provider as default
        let default_cfg = AppConfig {
            default_provider: Some(provider.to_string()),
            ..AppConfig::default()
        };
        let toml_str = toml::to_string_pretty(&default_cfg)?;
        std::fs::create_dir_all(config_path.parent().unwrap())?;
        std::fs::write(&config_path, toml_str)?;
        println!("  {} ~/.agenta/config.toml", "✓ Config written to".green());
    }

    // ── Start daemon if needed ────────────────────────────────────────────────
    let daemon_running = is_daemon_running(config).await;
    if !daemon_running {
        print!("  Starting daemon... ");
        std::io::stdout().flush()?;
        daemon_start(config, false).await?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        println!("{}", "✓".green());
    }

    // ── Reload config (picks up .env written above) ───────────────────────────
    let fresh_config = AppConfig::load().unwrap_or_else(|_| config.clone());

    // ── Create MIND ───────────────────────────────────────────────────────────
    let mut mind = Agent::new(
        crate::core::MIND_AGENT_NAME.to_string(),
        model.clone(),
        crate::core::MIND_SYSTEM_PROMPT.to_string(),
    );
    mind.is_system = true;
    mind.provider = Some(provider.to_string());
    mind.status = AgentStatus::Active;
    // MIND is a deep agent — the builder builtins (propose_create_tool) and its
    // multi-step reasoning only run in the deep-agent loop.
    mind.deep_agent_config = Some(DeepAgentConfig {
        max_iterations: 10,
        enable_reflection: true,
        available_tools: vec![],
        stop_conditions: vec!["task_complete".to_string()],
        allow_sub_agents: false,
        subagent_spawn_message: None,
    });

    let agent_value = serde_json::to_value(&mind)?;
    match daemon_request(&fresh_config, DaemonRequest::CreateAgent { agent: agent_value }).await? {
        DaemonResponse::Success { .. } => {
            println!("  {} MIND", "✓ Created system agent".green());
        }
        DaemonResponse::Error { message } => {
            return Err(anyhow!("Failed to create MIND: {}", message));
        }
        _ => return Err(anyhow!("Unexpected response from daemon")),
    }

    // ── Optional: Telegram ────────────────────────────────────────────────────
    println!("  Set up Telegram bot? [y/N]: ");
    print!("  ");
    std::io::stdout().flush()?;
    let mut tg = String::new();
    std::io::stdin().read_line(&mut tg)?;
    if tg.trim().eq_ignore_ascii_case("y") {
        setup_telegram(&fresh_config).await?;
    }

    // ── Done ─────────────────────────────────────────────────────────────────
    println!();
    println!("{}", "  All done!".bold().green());
    println!("  Provider : {}", provider.cyan());
    println!("  Model    : {}", model.cyan());
    println!();
    println!("  Run {} to open the dashboard.", "agenta".cyan().bold());
    println!();

    Ok(())
}

/// Path to the daemon's pidfile (written by the daemon next to its socket).
fn daemon_pid_file(config: &AppConfig) -> std::path::PathBuf {
    Path::new(&config.socket_path).with_extension("pid")
}

/// Whether any `agenta-daemon` process is currently alive. Catches orphans not
/// recorded in the pidfile (e.g. duplicates left by earlier failed restarts).
fn daemon_processes_exist() -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", "agenta-daemon"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Force-kill every `agenta-daemon` process to guarantee a clean slate.
fn kill_all_daemons() {
    let _ = std::process::Command::new("pkill")
        .args(["-9", "-f", "agenta-daemon"])
        .status();
}

/// Remove the daemon's socket + pidfile so a fresh start has a clean slate.
fn cleanup_daemon_files(config: &AppConfig) {
    let _ = std::fs::remove_file(&config.socket_path);
    let _ = std::fs::remove_file(daemon_pid_file(config));
}

async fn daemon_start(config: &AppConfig, foreground: bool) -> Result<()> {
    if is_daemon_running(config).await {
        println!("Daemon is already running");
        return Ok(());
    }

    // We got here because nothing is responding on the socket. If any daemon
    // process is still alive, it's an orphan (unresponsive, or holding ports
    // 8789/8790 without a working socket) — sweep it so we never spawn a second
    // daemon that collides.
    if daemon_processes_exist() {
        eprintln!("{}", "Found leftover daemon process(es); cleaning up...".yellow());
        kill_all_daemons();
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    cleanup_daemon_files(config);

    let data_dir = std::path::Path::new(&config.database_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    if foreground {
        println!("Starting daemon in foreground...");
        let daemon_bin = resolve_daemon_binary()?;
        let status = std::process::Command::new(daemon_bin).status()?;
        if !status.success() {
            return Err(anyhow!("Daemon exited with status: {}", status));
        }
    } else {
        // Start daemon in background
        println!("Starting daemon...");

        // Ensure data directory exists
        std::fs::create_dir_all(data_dir)?;

        // Start installed daemon binary directly (works outside repo and without cargo in PATH).
        // `process_group(0)` detaches it into its own process group so it survives the CLI
        // exiting or being killed — without this, killing the foreground `agenta daemon …`
        // command (or its job-control group) takes the daemon down with it.
        let daemon_bin = resolve_daemon_binary()?;
        use std::os::unix::process::CommandExt;
        let _child = std::process::Command::new(daemon_bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()?;

        // Wait for daemon to start
        for _ in 0..20 {
            if is_daemon_running(config).await {
                println!("{}", "Daemon started successfully".green());
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        return Err(anyhow!(
            "Daemon did not come up within timeout. Another process may be holding the socket \
             or ports 8789/8790. Inspect with `pgrep -fl agenta-daemon` and `lsof -i :8789`, \
             then run `agenta daemon stop` and try again."
        ));
    }

    Ok(())
}

async fn daemon_stop(config: &AppConfig) -> Result<()> {
    // Authoritative: after this returns, NO agenta-daemon process is running.
    if !is_daemon_running(config).await && !daemon_processes_exist() {
        cleanup_daemon_files(config);
        println!("Daemon is not running");
        return Ok(());
    }

    // Ask the daemon to shut down gracefully.
    println!("Shutting down...");
    let _ = daemon_request(config, DaemonRequest::Shutdown).await;

    // Wait until no daemon process survives — including untracked orphans
    // (duplicates from earlier failed restarts) the pidfile doesn't know about.
    // Key off the process, not the socket file: the daemon doesn't unlink its
    // socket on exit, so the file lingers and is cleaned up by cleanup_daemon_files.
    for _ in 0..40 {
        if !daemon_processes_exist() {
            cleanup_daemon_files(config);
            println!("{}", "Daemon stopped".green());
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Survivors remain (orphans holding ports, or graceful stop didn't take) —
    // force-kill every daemon to guarantee a clean slate.
    eprintln!("{}", "Forcing shutdown of remaining daemon process(es)...".yellow());
    kill_all_daemons();
    tokio::time::sleep(Duration::from_millis(500)).await;
    cleanup_daemon_files(config);
    println!("{}", "Daemon stopped".green());
    Ok(())
}

async fn handle_proposals_command(
    all: bool,
    command: Option<ProposalCommands>,
    config: &AppConfig,
) -> Result<()> {
    match command {
        None => list_proposals(config, all).await,
        Some(ProposalCommands::Show { id }) => show_proposal(config, &id).await,
    }
}

pub(crate) async fn list_proposals(config: &AppConfig, all: bool) -> Result<()> {
    // Default view is the pending queue — what needs a decision now.
    let status = if all { None } else { Some("pending".to_string()) };
    match daemon_request(config, DaemonRequest::ListProposals { status }).await? {
        DaemonResponse::ProposalList { proposals } => {
            let proposals: Vec<Proposal> = proposals
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            print_proposals_table(&proposals, all);
            Ok(())
        }
        DaemonResponse::Error { message } => Err(anyhow!(message)),
        _ => Err(anyhow!("Unexpected response")),
    }
}

async fn show_proposal(config: &AppConfig, id: &str) -> Result<()> {
    match daemon_request(config, DaemonRequest::GetProposal { id: id.to_string() }).await? {
        DaemonResponse::ProposalDetails { proposal } => {
            let proposal: Proposal = serde_json::from_value(proposal)?;
            print_proposal_detail(&proposal);
            Ok(())
        }
        DaemonResponse::Error { message } => Err(anyhow!(message)),
        _ => Err(anyhow!("Unexpected response")),
    }
}

async fn resolve_proposal(
    config: &AppConfig,
    id: &str,
    approve: bool,
    reason: Option<String>,
) -> Result<()> {
    let request = if approve {
        DaemonRequest::ApproveProposal { id: id.to_string() }
    } else {
        DaemonRequest::RejectProposal { id: id.to_string(), reason }
    };
    match daemon_request(config, request).await? {
        DaemonResponse::ProposalDetails { proposal } => {
            let proposal: Proposal = serde_json::from_value(proposal)?;
            match proposal.status {
                ProposalStatus::Applied => println!(
                    "{} {}",
                    "✓ Applied:".green(),
                    proposal.result.as_deref().unwrap_or(&proposal.summary())
                ),
                ProposalStatus::Failed => println!(
                    "{} {}",
                    "✗ Apply failed:".red(),
                    proposal.result.as_deref().unwrap_or("unknown error")
                ),
                ProposalStatus::Rejected => println!("{} {}", "Rejected:".yellow(), proposal.summary()),
                ProposalStatus::Pending => println!("Proposal still pending."),
            }
            Ok(())
        }
        DaemonResponse::Error { message } => Err(anyhow!(message)),
        _ => Err(anyhow!("Unexpected response")),
    }
}

/// Colored risk badge for list/detail views.
fn risk_badge(risk: crate::core::Risk) -> String {
    use crate::core::Risk;
    match risk {
        Risk::Low => "low".green().to_string(),
        Risk::Elevated => "elevated".yellow().to_string(),
        Risk::Destructive => "DESTRUCTIVE".red().bold().to_string(),
    }
}

fn print_proposals_table(proposals: &[Proposal], all: bool) {
    if proposals.is_empty() {
        let scope = if all { "proposals" } else { "pending proposals" };
        println!("No {}.", scope);
        return;
    }
    let mut table = styled_table();
    table.set_header(vec!["ID", "Risk", "Action", "By", "Status", "When"]);
    for p in proposals {
        table.add_row(vec![
            p.id.chars().take(8).collect::<String>(),
            risk_badge(p.risk),
            p.summary(),
            p.proposed_by.clone(),
            format!("{:?}", p.status),
            p.created_at.format("%Y-%m-%d %H:%M").to_string(),
        ]);
    }
    println!("{table}");
    if !all {
        println!(
            "{}",
            "Review with: agenta proposals show <id> · approve <id> · reject <id>".dimmed()
        );
    }
}

fn print_proposal_detail(p: &Proposal) {
    let mut table = styled_table();
    table.set_header(vec!["Property", "Value"]);
    table.add_row(vec!["ID".to_string(), p.id.clone()]);
    table.add_row(vec!["Action".to_string(), p.summary()]);
    table.add_row(vec!["Risk".to_string(), risk_badge(p.risk)]);
    table.add_row(vec!["Status".to_string(), format!("{:?}", p.status)]);
    table.add_row(vec!["Proposed by".to_string(), p.proposed_by.clone()]);
    table.add_row(vec!["Rationale".to_string(), p.rationale.clone()]);
    if let Some(result) = &p.result {
        table.add_row(vec!["Result".to_string(), result.clone()]);
    }
    println!("{table}");

    // The full payload — what will actually be created/changed on approval.
    println!("\n{}", "Payload (applied on approval):".bold());
    if let Ok(rendered) = serde_json::to_string_pretty(&p.action) {
        println!("{}", rendered);
    }
    if p.status == ProposalStatus::Pending {
        println!(
            "\n{}",
            format!("Approve: agenta approve {}   ·   Reject: agenta reject {}",
                &p.id[..8.min(p.id.len())], &p.id[..8.min(p.id.len())]).dimmed()
        );
    }
}

/// Parse the CLI `--side-effect` value into the enum. Accepts hyphen or
/// underscore spelling (read-only / read_only / readonly).
fn parse_side_effect(s: &str) -> Result<SideEffect> {
    match s.trim().to_lowercase().replace('-', "_").as_str() {
        "read_only" | "readonly" | "read" => Ok(SideEffect::ReadOnly),
        "write" => Ok(SideEffect::Write),
        "destructive" => Ok(SideEffect::Destructive),
        other => Err(anyhow!(
            "Invalid --side-effect '{}': expected read-only | write | destructive",
            other
        )),
    }
}

/// Parse `--http-header "Key: Value"` args into a header map.
fn parse_http_headers(raw: &[String]) -> Result<std::collections::HashMap<String, String>> {
    let mut headers = std::collections::HashMap::new();
    for h in raw {
        let (k, v) = h
            .split_once(':')
            .ok_or_else(|| anyhow!("Invalid --http-header '{}': expected 'Key: Value'", h))?;
        headers.insert(k.trim().to_string(), v.trim().to_string());
    }
    Ok(headers)
}

async fn handle_tool_command(command: ToolCommands, config: &AppConfig) -> Result<()> {
    match command {
        ToolCommands::Create {
            name,
            description,
            parameters,
            handler,
            scaffold,
            secrets,
            side_effect,
            http,
            http_method,
            http_headers,
        } => {
            let parameters: serde_json::Value = serde_json::from_str(&parameters)
                .map_err(|e| anyhow!("Invalid --parameters JSON: {}", e))?;
            let side_effect = parse_side_effect(&side_effect)?;

            // HTTP tool: --handler is the URL, no script is scaffolded.
            let (resolved_handler, http_config) = if http {
                let url = handler.ok_or_else(|| {
                    anyhow!("--http requires --handler <URL> (the request endpoint)")
                })?;
                let cfg = HttpHandler {
                    method: http_method,
                    headers: parse_http_headers(&http_headers)?,
                };
                (Some(url), Some(cfg))
            } else {
                let should_scaffold = scaffold || handler.is_none();
                let h = if should_scaffold {
                    Some(scaffold_tool_handler(&name, handler.as_deref())?)
                } else {
                    handler
                };
                (h, None)
            };

            let mut tool = ToolResource::new(name, description, parameters, resolved_handler);
            tool.secrets = secrets;
            tool.side_effect = side_effect;
            tool.http = http_config;
            let request = DaemonRequest::CreateTool {
                tool: serde_json::to_value(tool)?,
            };
            match daemon_request(config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }
        ToolCommands::Get { id } => {
            let request = DaemonRequest::GetTool { id };
            match daemon_request(config, request).await? {
                DaemonResponse::ToolDetails { tool } => {
                    let tool: ToolResource = serde_json::from_value(tool)?;
                    print_tool_details(&tool);
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }
        ToolCommands::List => {
            let request = DaemonRequest::ListTools;
            match daemon_request(config, request).await? {
                DaemonResponse::ToolList { tools } => {
                    let tools: Vec<ToolResource> = tools
                        .into_iter()
                        .filter_map(|v| serde_json::from_value(v).ok())
                        .collect();
                    print_tools_table(&tools);
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }
        ToolCommands::Update {
            id,
            name,
            description,
            parameters,
            handler,
            enabled,
            secrets,
            side_effect,
            http_method,
            http_headers,
        } => {
            let current = match daemon_request(config, DaemonRequest::GetTool { id: id.clone() }).await? {
                DaemonResponse::ToolDetails { tool } => serde_json::from_value::<ToolResource>(tool)?,
                DaemonResponse::Error { message } => return Err(anyhow!(message)),
                _ => return Err(anyhow!("Unexpected response")),
            };
            let mut tool = current;
            if let Some(v) = name { tool.name = v; }
            if let Some(v) = description { tool.description = v; }
            if let Some(v) = parameters {
                tool.parameters = serde_json::from_str(&v)
                    .map_err(|e| anyhow!("Invalid --parameters JSON: {}", e))?;
            }
            if handler.is_some() {
                tool.handler = handler;
            }
            if let Some(v) = enabled { tool.enabled = v; }
            // Empty `secrets` means "not provided" — leave the allowlist untouched.
            if !secrets.is_empty() { tool.secrets = secrets; }
            if let Some(v) = side_effect { tool.side_effect = parse_side_effect(&v)?; }
            // Setting method or headers makes (or updates) this an HTTP tool.
            if http_method.is_some() || !http_headers.is_empty() {
                let mut cfg = tool.http.take().unwrap_or_else(|| HttpHandler {
                    method: "POST".to_string(),
                    headers: std::collections::HashMap::new(),
                });
                if let Some(m) = http_method { cfg.method = m; }
                if !http_headers.is_empty() { cfg.headers = parse_http_headers(&http_headers)?; }
                tool.http = Some(cfg);
            }

            let request = DaemonRequest::UpdateTool {
                id,
                tool: serde_json::to_value(tool)?,
            };
            match daemon_request(config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }
        ToolCommands::Delete { id } => {
            let request = DaemonRequest::DeleteTool { id };
            match daemon_request(config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }
        ToolCommands::Run { id, input, wait, yes } => {
            let input: serde_json::Value = serde_json::from_str(&input)
                .map_err(|e| anyhow!("Invalid --input JSON: {}", e))?;

            // Guard: confirm before manually running a state-changing tool. Only
            // prompts on an interactive terminal; `--yes` or a non-TTY skips it.
            if !yes && std::io::stdin().is_terminal() {
                if let DaemonResponse::ToolDetails { tool } =
                    daemon_request(config, DaemonRequest::GetTool { id: id.clone() }).await?
                {
                    if let Ok(tool) = serde_json::from_value::<ToolResource>(tool) {
                        if tool.side_effect != SideEffect::ReadOnly {
                            print!(
                                "⚠ '{}' is a {:?} tool. Run it? [y/N] ",
                                tool.name, tool.side_effect
                            );
                            std::io::stdout().flush().ok();
                            let mut line = String::new();
                            std::io::stdin().read_line(&mut line)?;
                            if !matches!(line.trim().to_lowercase().as_str(), "y" | "yes") {
                                println!("Aborted.");
                                return Ok(());
                            }
                        }
                    }
                }
            }

            let request = DaemonRequest::RunTool { id, input };
            match daemon_request(config, request).await? {
                DaemonResponse::ToolExecutionStarted { execution_id } => {
                    println!("Tool execution started: {}", execution_id.blue());
                    if wait {
                        wait_for_tool_execution(config, &execution_id).await?;
                    }
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }
        ToolCommands::Logs {
            tool_id,
            execution_id,
            lines,
            follow,
        } => {
            let request = DaemonRequest::GetToolLogs {
                tool_id,
                execution_id,
                lines,
            };
            if follow {
                follow_tool_logs(config, request).await
            } else {
                match daemon_request(config, request).await? {
                    DaemonResponse::ToolExecutionLog { lines } => {
                        for line in lines {
                            println!("{}", line);
                        }
                        Ok(())
                    }
                    DaemonResponse::Error { message } => Err(anyhow!(message)),
                    _ => Err(anyhow!("Unexpected response")),
                }
            }
        }
    }
}

fn resolve_daemon_binary() -> Result<std::path::PathBuf> {
    let current = std::env::current_exe()?;
    if let Some(dir) = current.parent() {
        let sibling = dir.join("agenta-daemon");
        if sibling.exists() {
            return Ok(sibling);
        }
    }
    Ok(std::path::PathBuf::from("agenta-daemon"))
}

/// Shared table style used across every CLI table (list + detail views), so the
/// UI stays consistent: condensed UTF-8 borders + dynamic wrapping to terminal width.
fn styled_table() -> Table {
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_FULL_CONDENSED);
    table.set_content_arrangement(comfy_table::ContentArrangement::Dynamic);
    table
}

fn print_agent_details(agent: &Agent) {
    let mut table = styled_table();
    table.set_header(vec!["Property", "Value"]);

    table.add_row(vec!["ID", &agent.id]);
    table.add_row(vec!["Name", &agent.name]);
    table.add_row(vec![
        "Description",
        agent.description.as_deref().unwrap_or("N/A"),
    ]);
    table.add_row(vec!["Model", &agent.model]);
    if let Some(provider) = &agent.provider {
        table.add_row(vec!["Provider", provider]);
    }
    table.add_row(vec!["Status", &format!("{:?}", agent.status)]);
    table.add_row(vec!["Execution Mode", &format!("{:?}", agent.execution_mode)]);

    if let Some(schedule) = &agent.schedule {
        table.add_row(vec!["Schedule", schedule]);
    }

    table.add_row(vec!["Temperature", &agent.config.temperature.to_string()]);
    table.add_row(vec!["Max Tokens", &agent.config.max_tokens.to_string()]);
    table.add_row(vec!["Created", &agent.created_at.to_rfc3339()]);
    table.add_row(vec!["Updated", &agent.updated_at.to_rfc3339()]);

    if agent.is_deep_agent() {
        table.add_row(vec!["Deep Agent", "Yes"]);
    }
    table.add_row(vec!["Memory", if agent.memory_enabled { "Enabled" } else { "Disabled" }]);

    // Only surfaced when enabled — an opt-in safety flag, off for virtually all agents.
    if agent.config.allow_destructive_tools {
        table.add_row(vec!["Destructive Tools", "Allowed (autonomous)"]);
    }

    // RAG rows only appear when knowledge bases are attached — their presence is
    // what marks this as a RAG agent (no separate/duplicative flag needed).
    if !agent.config.knowledge_bases.is_empty() {
        let kbs = agent.config.knowledge_bases.join(", ");
        table.add_row(vec!["Knowledge Bases", &kbs]);
        let top_k = match agent.config.rag_top_k {
            Some(k) => format!("{} (override)", k),
            None => "8 (default)".to_string(),
        };
        table.add_row(vec!["RAG Top-K", &top_k]);
    }

    println!("{}", table);

    println!("\n{}", "System Prompt:".bold());
    println!("{}", agent.system_prompt);

    if !agent.tools.is_empty() {
        println!("\n{}", "Tools:".bold());
        for tool in &agent.tools {
            println!("  - {}: {}", tool.name, tool.description);
        }
    }
}

fn print_agents_table(agents: &[Agent], filter_status: Option<&str>) {
    if agents.is_empty() {
        println!("No agents found");
        return;
    }

    let mut table = styled_table();
    table.set_header(vec!["Name", "Model", "Mode", "Status", "Runs", "RAG", "Last Run"]);

    for agent in agents {
        if let Some(status) = filter_status {
            if !format!("{:?}", agent.status).to_lowercase().contains(status) {
                continue;
            }
        }

        let last_run = agent
            .last_run
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "Never".to_string());

        // Keep plain text in table cells to avoid ANSI escape sequences
        // affecting visual column sizing in some terminals.
        let status_str = format!("{:?}", agent.status);
        // "KB" marks agents with knowledge bases attached (RAG agents).
        let rag = if agent.config.knowledge_bases.is_empty() { "" } else { "KB" };

        table.add_row(vec![
            Cell::new(agent.name.clone()),
            Cell::new(agent.model.clone()),
            Cell::new(format!("{:?}", agent.execution_mode)),
            Cell::new(status_str),
            Cell::new(format!("{:02}", agent.run_count)).set_alignment(CellAlignment::Right),
            Cell::new(rag).set_alignment(CellAlignment::Center),
            Cell::new(last_run),
        ]);
    }

    println!("{}", table);
}

fn parse_execution_mode(mode: &str) -> Result<ExecutionMode> {
    match mode.to_lowercase().as_str() {
        "once" => Ok(ExecutionMode::Once),
        "scheduled" => Ok(ExecutionMode::Scheduled),
        "triggered" => Ok(ExecutionMode::Triggered),
        "continuous" => Ok(ExecutionMode::Continuous),
        _ => Err(anyhow!("Invalid execution mode: {}", mode)),
    }
}

fn read_tool_definitions(tools_arg: &str) -> Result<Vec<ToolDefinition>> {
    let mut tools = Vec::new();
    for raw_path in tools_arg.split(',') {
        let path = raw_path.trim();
        if path.is_empty() {
            continue;
        }

        let content = std::fs::read_to_string(path)?;
        let value: serde_json::Value = if path.ends_with(".yaml") || path.ends_with(".yml") {
            serde_yaml::from_str(&content)?
        } else {
            serde_json::from_str(&content)?
        };

        if let Some(arr) = value.as_array() {
            for item in arr {
                let tool: ToolDefinition = serde_json::from_value(item.clone())?;
                tools.push(tool);
            }
            continue;
        }

        if let Some(arr) = value.get("tools").and_then(|v| v.as_array()) {
            for item in arr {
                let tool: ToolDefinition = serde_json::from_value(item.clone())?;
                tools.push(tool);
            }
            continue;
        }

        let tool: ToolDefinition = serde_json::from_value(value)?;
        tools.push(tool);
    }

    Ok(tools)
}

fn write_export(path: &str, data: &serde_json::Value, format: &str) -> Result<()> {
    let content = match format.to_lowercase().as_str() {
        "yaml" | "yml" => serde_yaml::to_string(data)?,
        _ => serde_json::to_string_pretty(data)?,
    };
    std::fs::write(path, content)?;
    Ok(())
}

async fn wait_for_execution(config: &AppConfig, execution_id: &str) -> Result<()> {
    let started = std::time::Instant::now();
    let timeout = Duration::from_secs(15 * 60);
    let not_found_timeout = Duration::from_secs(20);
    let mut last_status = String::new();
    let mut next_heartbeat = std::time::Instant::now();
    let mut not_found_since: Option<std::time::Instant> = None;

    loop {
        if started.elapsed() > timeout {
            return Err(anyhow!(
                "Timed out waiting for execution {}. Check with: agenta logs <agent> --execution-id {}",
                execution_id,
                execution_id
            ));
        }

        let request = DaemonRequest::GetExecution {
            id: execution_id.to_string(),
        };
        match daemon_request(config, request).await? {
            DaemonResponse::ExecutionResult { result } => {
                not_found_since = None;
                let status_value = result.get("status").cloned().unwrap_or(serde_json::Value::Null);
                let status = match status_value {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                let status = status.trim_matches('"').to_lowercase();
                let completed = result
                    .get("completed_at")
                    .map(|v| !v.is_null())
                    .unwrap_or(false);

                if status != last_status {
                    println!("Execution {} status: {}", execution_id, status);
                    last_status = status.clone();
                }

                if completed
                    || status.contains("completed")
                    || status.contains("failed")
                    || status.contains("cancelled")
                {
                    if let Some(output) = result.get("output").and_then(|v| v.as_str()) {
                        println!("{}", output);
                    }
                    if let Some(error) = result.get("error").and_then(|v| v.as_str()) {
                        eprintln!("{}", error);
                    }
                    break;
                }
            }
            DaemonResponse::Error { message } => {
                if message.to_lowercase().contains("execution not found") {
                    let since = not_found_since.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() > not_found_timeout {
                        return Err(anyhow!(
                            "Execution {} was never created in daemon storage after {}s. Check daemon logs for execution startup errors.",
                            execution_id,
                            not_found_timeout.as_secs()
                        ));
                    }
                    if std::time::Instant::now() >= next_heartbeat {
                        println!("Waiting for execution record {}...", execution_id);
                        next_heartbeat = std::time::Instant::now() + Duration::from_secs(3);
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                return Err(anyhow!("{}", message));
            }
            _ => {}
        }

        tokio::time::sleep(Duration::from_millis(750)).await;
        if std::time::Instant::now() >= next_heartbeat {
            println!("Execution {} still running...", execution_id);
            next_heartbeat = std::time::Instant::now() + Duration::from_secs(5);
        }
    }

    Ok(())
}

async fn follow_logs(config: &AppConfig, request: DaemonRequest) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    loop {
        match daemon_request(config, request.clone()).await? {
            DaemonResponse::ExecutionLog { lines } => {
                for line in lines {
                    if seen.insert(line.clone()) {
                        println!("{}", line);
                    }
                }
            }
            DaemonResponse::Error { message } => return Err(anyhow!("{}", message)),
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

fn print_tool_details(tool: &ToolResource) {
    let mut table = styled_table();
    table.set_header(vec!["Property", "Value"]);
    table.add_row(vec!["ID", &tool.id]);
    table.add_row(vec!["Name", &tool.name]);
    table.add_row(vec!["Description", &tool.description]);
    table.add_row(vec!["Enabled", if tool.enabled { "true" } else { "false" }]);
    table.add_row(vec!["Side Effect", &format!("{:?}", tool.side_effect)]);
    let secrets = if tool.secrets.is_empty() {
        "none".to_string()
    } else {
        tool.secrets.join(", ")
    };
    table.add_row(vec!["Secrets", &secrets]);
    if let Some(http) = &tool.http {
        table.add_row(vec!["Type", &format!("HTTP ({})", http.method)]);
        table.add_row(vec!["URL", tool.handler.as_deref().unwrap_or("N/A")]);
        if !http.headers.is_empty() {
            let hdrs = http
                .headers
                .iter()
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect::<Vec<_>>()
                .join("\n");
            table.add_row(vec!["Headers", &hdrs]);
        }
    } else {
        table.add_row(vec!["Type", "script"]);
        table.add_row(vec!["Handler", tool.handler.as_deref().unwrap_or("N/A")]);
    }
    table.add_row(vec!["Created", &tool.created_at.to_rfc3339()]);
    table.add_row(vec!["Updated", &tool.updated_at.to_rfc3339()]);
    println!("{}", table);
    println!("\nParameters schema:");
    let rendered = serde_json::to_string_pretty(&tool.parameters)
        .unwrap_or_else(|_| tool.parameters.to_string());
    println!("{}", rendered);
}

fn print_tools_table(tools: &[ToolResource]) {
    if tools.is_empty() {
        println!("No tools found");
        return;
    }
    let mut table = styled_table();
    table.set_header(vec!["Name", "Enabled", "Handler", "Updated"]);
    for tool in tools {
        table.add_row(vec![
            tool.name.clone(),
            if tool.enabled { "yes".to_string() } else { "no".to_string() },
            tool.handler.clone().unwrap_or_else(|| "N/A".to_string()),
            tool.updated_at.format("%Y-%m-%d %H:%M").to_string(),
        ]);
    }
    println!("{}", table);
}

fn print_executions_table(executions: &[ExecutionResult]) {
    if executions.is_empty() {
        println!("No executions found");
        return;
    }

    let mut table = styled_table();
    table.set_header(vec!["Execution ID", "Agent ID", "Status", "Started", "Completed", "Error"]);

    for execution in executions {
        let completed = execution
            .completed_at
            .map(|ts| ts.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".to_string());
        let error = execution.error.clone().unwrap_or_else(|| "-".to_string());

        table.add_row(vec![
            execution.id.clone(),
            execution.agent_id.clone(),
            format!("{:?}", execution.status).to_lowercase(),
            execution.started_at.format("%Y-%m-%d %H:%M").to_string(),
            completed,
            error,
        ]);
    }

    println!("{}", table);
}

async fn wait_for_tool_execution(config: &AppConfig, execution_id: &str) -> Result<()> {
    let started = std::time::Instant::now();
    let timeout = Duration::from_secs(10 * 60);
    let mut last_status = String::new();
    let mut next_heartbeat = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if started.elapsed() > timeout {
            return Err(anyhow!("Timed out waiting for tool execution {}", execution_id));
        }
        let request = DaemonRequest::GetToolExecution {
            id: execution_id.to_string(),
        };
        match daemon_request(config, request).await? {
            DaemonResponse::ToolExecutionResult { result } => {
                let status_value = result
                    .get("status")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let status = match status_value {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                let status = status.trim_matches('"').to_lowercase();
                let completed = result
                    .get("completed_at")
                    .map(|v| !v.is_null())
                    .unwrap_or(false);

                if status != last_status {
                    println!("Tool execution {} status: {}", execution_id, status);
                    last_status = status.clone();
                }

                if completed || status.contains("completed") || status.contains("failed") {
                    if let Some(output) = result.get("output").and_then(|v| v.as_str()) {
                        println!("{}", output);
                    }
                    if let Some(error) = result.get("error").and_then(|v| v.as_str()) {
                        eprintln!("{}", error);
                    }
                    break;
                }
            }
            DaemonResponse::Error { message } => return Err(anyhow!(message)),
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        if std::time::Instant::now() >= next_heartbeat {
            println!("Tool execution {} still running...", execution_id);
            next_heartbeat = std::time::Instant::now() + Duration::from_secs(5);
        }
    }
    Ok(())
}

async fn follow_tool_logs(config: &AppConfig, request: DaemonRequest) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    loop {
        match daemon_request(config, request.clone()).await? {
            DaemonResponse::ToolExecutionLog { lines } => {
                for line in lines {
                    if seen.insert(line.clone()) {
                        println!("{}", line);
                    }
                }
            }
            DaemonResponse::Error { message } => return Err(anyhow!(message)),
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

async fn handle_script_command(command: ScriptCommands, config: &AppConfig) -> Result<()> {
    match command {
        ScriptCommands::Create { name, handler, description, schedule } => {
            let script = ScriptDefinition::new(name, handler, description, schedule);
            let request = DaemonRequest::CreateScript {
                script: serde_json::to_value(script)?,
            };
            match daemon_request(config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        ScriptCommands::Get { id } => {
            let request = DaemonRequest::GetScript { id };
            match daemon_request(config, request).await? {
                DaemonResponse::ScriptDetails { script } => {
                    let script: ScriptDefinition = serde_json::from_value(script)?;
                    print_script_details(&script);
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        ScriptCommands::List => {
            let request = DaemonRequest::ListScripts;
            match daemon_request(config, request).await? {
                DaemonResponse::ScriptList { scripts } => {
                    let scripts: Vec<ScriptDefinition> = scripts
                        .into_iter()
                        .filter_map(|v| serde_json::from_value(v).ok())
                        .collect();
                    print_scripts_table(&scripts);
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        ScriptCommands::Update { id, name, handler, description, schedule, enabled } => {
            let current = match daemon_request(config, DaemonRequest::GetScript { id: id.clone() }).await? {
                DaemonResponse::ScriptDetails { script } => serde_json::from_value::<ScriptDefinition>(script)?,
                DaemonResponse::Error { message } => return Err(anyhow!(message)),
                _ => return Err(anyhow!("Unexpected response")),
            };
            let mut script = current;
            if let Some(v) = name { script.name = v; }
            if let Some(v) = handler { script.handler = v; }
            if description.is_some() { script.description = description; }
            if schedule.is_some() { script.schedule = schedule; }
            if let Some(v) = enabled { script.enabled = v; }

            let request = DaemonRequest::UpdateScript {
                id,
                script: serde_json::to_value(script)?,
            };
            match daemon_request(config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        ScriptCommands::Delete { id, force } => {
            if !force {
                print!("Are you sure you want to delete script {}? [y/N] ", id);
                std::io::stdout().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if !input.trim().eq_ignore_ascii_case("y") {
                    println!("Cancelled");
                    return Ok(());
                }
            }
            let request = DaemonRequest::DeleteScript { id };
            match daemon_request(config, request).await? {
                DaemonResponse::Success { message } => {
                    println!("{}", message.green());
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        ScriptCommands::Run { id, wait } => {
            let request = DaemonRequest::RunScript { id: id.clone() };
            match daemon_request(config, request).await? {
                DaemonResponse::ScriptExecutionStarted { execution_id } => {
                    println!("Script execution started: {}", execution_id.blue());
                    if wait {
                        wait_for_script_execution(config, &id, &execution_id).await?;
                    }
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }

        ScriptCommands::Logs { script_id, execution_id, lines } => {
            let request = DaemonRequest::GetScriptLogs {
                script_id,
                execution_id,
                lines,
            };
            match daemon_request(config, request).await? {
                DaemonResponse::ScriptExecutionLog { lines } => {
                    for line in lines {
                        println!("{}", line);
                    }
                    Ok(())
                }
                DaemonResponse::Error { message } => Err(anyhow!(message)),
                _ => Err(anyhow!("Unexpected response")),
            }
        }
    }
}

fn print_script_details(script: &ScriptDefinition) {
    let mut table = styled_table();
    table.set_header(vec!["Property", "Value"]);
    table.add_row(vec!["ID", &script.id]);
    table.add_row(vec!["Name", &script.name]);
    table.add_row(vec![
        "Description",
        script.description.as_deref().unwrap_or("N/A"),
    ]);
    table.add_row(vec!["Handler", &script.handler]);
    table.add_row(vec![
        "Schedule",
        script.schedule.as_deref().unwrap_or("None"),
    ]);
    table.add_row(vec!["Enabled", if script.enabled { "yes" } else { "no" }]);
    table.add_row(vec!["Run Count", &script.run_count.to_string()]);
    table.add_row(vec![
        "Last Run",
        &script
            .last_run
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "Never".to_string()),
    ]);
    table.add_row(vec!["Created", &script.created_at.to_rfc3339()]);
    table.add_row(vec!["Updated", &script.updated_at.to_rfc3339()]);
    println!("{}", table);
}

fn print_scripts_table(scripts: &[ScriptDefinition]) {
    if scripts.is_empty() {
        println!("No scripts found");
        return;
    }
    let mut table = styled_table();
    table.set_header(vec!["Name", "Schedule", "Enabled", "Runs", "Last Run", "Handler"]);
    for script in scripts {
        table.add_row(vec![
            script.name.clone(),
            script.schedule.clone().unwrap_or_else(|| "None".to_string()),
            if script.enabled { "yes".to_string() } else { "no".to_string() },
            script.run_count.to_string(),
            script
                .last_run
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "Never".to_string()),
            script.handler.clone(),
        ]);
    }
    println!("{}", table);
}

async fn wait_for_script_execution(
    config: &AppConfig,
    script_id: &str,
    execution_id: &str,
) -> Result<()> {
    let started = std::time::Instant::now();
    let timeout = Duration::from_secs(10 * 60);
    let mut next_heartbeat = std::time::Instant::now() + Duration::from_secs(5);

    loop {
        if started.elapsed() > timeout {
            return Err(anyhow!("Timed out waiting for script execution {}", execution_id));
        }

        let request = DaemonRequest::GetScriptLogs {
            script_id: script_id.to_string(),
            execution_id: Some(execution_id[..8.min(execution_id.len())].to_string()),
            lines: 100,
        };

        match daemon_request(config, request).await? {
            DaemonResponse::ScriptExecutionLog { lines } => {
                let is_done = lines.iter().any(|l| {
                    l.contains("Completed") || l.contains("Failed") || l.contains("completed") || l.contains("failed")
                });
                if is_done {
                    for line in lines {
                        println!("{}", line);
                    }
                    break;
                }
            }
            DaemonResponse::Error { message } => return Err(anyhow!(message)),
            _ => {}
        }

        tokio::time::sleep(Duration::from_millis(750)).await;
        if std::time::Instant::now() >= next_heartbeat {
            println!("Script execution {} still running...", execution_id);
            next_heartbeat = std::time::Instant::now() + Duration::from_secs(5);
        }
    }
    Ok(())
}

pub(crate) fn scaffold_tool_handler(name: &str, handler_arg: Option<&str>) -> Result<String> {
    let path = if let Some(handler) = handler_arg {
        let script_path = handler
            .strip_prefix("/usr/bin/env bash ")
            .unwrap_or(handler)
            .trim();
        std::path::PathBuf::from(script_path)
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".agenta")
            .join("tools")
            .join(format!("{}.sh", name))
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if !path.exists() {
        let template = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail

# Tool: {name}
# Input: JSON via stdin or AGENTA_TOOL_PARAMS env var
INPUT="${{AGENTA_TOOL_PARAMS:-}}"
if [ -z "$INPUT" ]; then
  INPUT="$(cat)"
fi

# TODO: implement logic.
# Must print plain text (or JSON string) to stdout.
echo "tool {name} received: $INPUT"
"#
        );
        std::fs::write(&path, template)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms)?;
        }
    }

    Ok(format!("/usr/bin/env bash {}", path.display()))
}

pub(crate) fn read_installed_tool(name: &str) -> Result<ToolDefinition> {
    let install_dir = dirs::home_dir()
        .ok_or_else(|| anyhow!("Cannot determine home directory"))?
        .join(".agenta/tools")
        .join(name);

    let manifest_path = install_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err(anyhow!(
            "Tool '{}' is not installed. Run: agenta pull tool {}",
            name, name
        ));
    }

    let content = std::fs::read_to_string(&manifest_path)?;
    let value: serde_json::Value = serde_json::from_str(&content)?;

    let tool_name = value.get("name").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("manifest.json missing 'name'"))?
        .to_string();
    let description = value.get("description").and_then(|v| v.as_str())
        .unwrap_or("").to_string();
    let parameters = value.get("parameters").cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
    let handler_file = value.get("handler").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("manifest.json missing 'handler'"))?;

    let handler_path = install_dir.join(handler_file);
    let handler = format!("/usr/bin/env bash {}", handler_path.display());

    Ok(ToolDefinition {
        name: tool_name,
        description,
        parameters,
        handler: Some(handler),
        secrets: Vec::new(),
        side_effect: Default::default(),
        http: None,
        timeout_secs: None,
    })
}

async fn pull_tool(config: &AppConfig, name: &str, version: &str, attach: Option<&str>) -> Result<()> {
    let base = format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        config.registry_owner, config.registry_repo, version, name
    );

    println!("Pulling tool {} from registry ({}@{})...", name.cyan(), config.registry_repo, version);

    // 1. Fetch manifest
    let client = reqwest::Client::new();
    let manifest_url = format!("{}/manifest.json", base);
    let resp = client
        .get(&manifest_url)
        .send()
        .await
        .context("Failed to reach registry")?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!("Tool '{}' not found in registry. Check available tools at https://github.com/{}/{}", name, config.registry_owner, config.registry_repo));
    }
    if !resp.status().is_success() {
        return Err(anyhow!("Registry returned {}", resp.status()));
    }

    let manifest: serde_json::Value = resp.json().await.context("Invalid manifest.json")?;

    let handler_file = manifest
        .get("handler")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("manifest.json missing 'handler' field"))?
        .to_string();

    let description = manifest
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let env_vars: Vec<String> = manifest
        .get("env")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    // 2. Fetch handler script
    let handler_url = format!("{}/{}", base, handler_file);
    let script_bytes = client
        .get(&handler_url)
        .send()
        .await
        .context("Failed to download handler script")?
        .bytes()
        .await?;

    // 3. Write to ~/.agenta/tools/<name>/
    let install_dir = dirs::home_dir()
        .ok_or_else(|| anyhow!("Cannot determine home directory"))?
        .join(".agenta/tools")
        .join(name);

    tokio::fs::create_dir_all(&install_dir).await?;

    // Write manifest
    let manifest_path = install_dir.join("manifest.json");
    tokio::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?).await?;

    // Write handler script
    let handler_path = install_dir.join(&handler_file);
    tokio::fs::write(&handler_path, &script_bytes).await?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&handler_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&handler_path, perms)?;
    }

    // 4. Register as ToolResource in the daemon DB
    let parameters = manifest
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
    let tool_val = serde_json::json!({
        "name": name,
        "description": description,
        "parameters": parameters,
        "handler": format!("/usr/bin/env bash {}", handler_path.display()),
    });
    if let Err(e) = daemon_request(config, DaemonRequest::CreateTool { tool: tool_val }).await {
        // Non-fatal — daemon may not be running, files are still on disk
        eprintln!("{} Could not register with daemon: {}", "!".yellow(), e);
    }

    println!("{} Tool installed: {}", "✓".green(), name.cyan());
    println!("  Location : {}", install_dir.display());
    println!("  Handler  : {}", handler_path.display());
    if !description.is_empty() {
        println!("  About    : {}", description);
    }

    if !env_vars.is_empty() {
        println!("\n{} Required environment variables (add to ~/.agenta/.env):", "!".yellow());
        for var in &env_vars {
            let home = dirs::home_dir().unwrap_or_default();
            let env_file = home.join(".agenta/.env");
            let already_set = std::fs::read_to_string(&env_file)
                .map(|s| s.contains(var.as_str()))
                .unwrap_or(false);
            if already_set {
                println!("  {} {} (already set)", "✓".green(), var);
            } else {
                println!("  {} {} {}", "✗".red(), var, "(not set)".dimmed());
            }
        }
    }

    if let Some(agent_name) = attach {
        println!("\nAttaching to {}...", agent_name.cyan());
        attach_tool_to_agent(config, name, agent_name).await?;
    } else {
        println!("\nAttach to an agent:");
        println!("  agenta pull tool {} --attach <agent>", name);
        println!("  agenta update <agent> --add-tool {}", name);
    }

    Ok(())
}

async fn attach_tool_to_agent(config: &AppConfig, tool_name: &str, agent_name: &str) -> Result<()> {
    let tool = read_installed_tool(tool_name)?;

    let get_request = DaemonRequest::GetAgent { id: agent_name.to_string() };
    let mut agent: Agent = match daemon_request(config, get_request).await? {
        DaemonResponse::AgentDetails { agent } => serde_json::from_value(agent)?,
        DaemonResponse::Error { message } => return Err(anyhow!("{}", message)),
        _ => return Err(anyhow!("Unexpected response")),
    };

    let action = if let Some(pos) = agent.tools.iter().position(|t| t.name == tool.name) {
        agent.tools[pos] = tool;
        "updated"
    } else {
        agent.tools.push(tool);
        "added"
    };

    if let Some(cfg) = agent.deep_agent_config.as_mut() {
        cfg.available_tools = agent.tools.iter().map(|t| t.name.clone()).collect();
    }

    agent.touch();

    match daemon_request(config, DaemonRequest::UpdateAgent {
        id: agent.id.clone(),
        agent: serde_json::to_value(agent)?,
    }).await? {
        DaemonResponse::Success { .. } => {
            println!("{} Tool '{}' {} on agent {}", "✓".green(), tool_name.cyan(), action, agent_name.cyan());
            Ok(())
        }
        DaemonResponse::Error { message } => Err(anyhow!("{}", message)),
        _ => Err(anyhow!("Unexpected response")),
    }
}
