use anyhow::Result;
use owo_colors::OwoColorize;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use crate::core::{AppConfig, DaemonRequest, DaemonResponse};
use super::commands::daemon_request;

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run_shell(config: AppConfig) -> Result<()> {
    print_banner();

    let mut rl = DefaultEditor::new()?;

    // Load history
    let history_path = history_file();
    if let Some(ref p) = history_path {
        let _ = rl.load_history(p);
    }

    loop {
        match rl.readline("agenta> ") {
            Ok(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(&line);

                match dispatch(&line, &config).await {
                    Ok(true) => break,  // /quit
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("{}", format!("Error: {e}").red());
                        // Check if daemon is down and give a hint
                        let msg = e.to_string();
                        if msg.contains("not running") || msg.contains("connect") {
                            eprintln!("{}", "  Hint: run `agenta daemon start` first".dimmed());
                        }
                    }
                }
            }
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("Readline error: {e}");
                break;
            }
        }
    }

    if let Some(ref p) = history_path {
        let _ = rl.save_history(p);
    }

    println!("Goodbye.");
    Ok(())
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

async fn dispatch(input: &str, config: &AppConfig) -> Result<bool> {
    match input {
        "/quit" | "/exit" | "/q" => return Ok(true),
        "/help" | "/h"           => print_help(),
        "/list"                  => cmd_list(config).await?,
        "/list-tools"            => cmd_list_tools(config).await?,
        "/list-scripts"          => println!("{}", "Scripts coming soon.".dimmed()),
        "/status"                => cmd_status(config).await?,
        "/create-agent"          => wizard_create_agent(config).await?,
        "/update-agent"          => wizard_update_agent(config).await?,
        "/create-tool"           => wizard_create_tool(config).await?,
        "/update-tool"           => println!("{}", "Coming soon.".dimmed()),
        "/create-script"         => println!("{}", "Scripts coming soon.".dimmed()),
        "/get"                   => picker_get(config).await?,
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
    use comfy_table::{Table, Cell, CellAlignment};

    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_header(vec![
        Cell::new("Command").add_attribute(comfy_table::Attribute::Bold),
        Cell::new("Description").add_attribute(comfy_table::Attribute::Bold),
    ]);

    let commands = vec![
        ("/create-agent",  "Create a new agent (guided wizard)"),
        ("/update-agent",  "Update an existing agent"),
        ("/create-tool",   "Create a new tool (guided wizard)"),
        ("/update-tool",   "Update an existing tool"),
        ("",               ""),
        ("/list",          "List all agents"),
        ("/list-tools",    "List all tools"),
        ("/get",           "Show agent details"),
        ("/run",           "Run an agent"),
        ("/stop",          "Stop a running agent"),
        ("/logs",          "View agent logs"),
        ("/delete",        "Delete an agent"),
        ("",               ""),
        ("/status",        "Show daemon status"),
        ("/help",          "Show this help"),
        ("/quit",          "Exit the shell"),
    ];

    for (cmd, desc) in commands {
        if cmd.is_empty() {
            table.add_row(vec![Cell::new(""), Cell::new("")]);
        } else {
            table.add_row(vec![
                Cell::new(cmd).fg(comfy_table::Color::Cyan),
                Cell::new(desc),
            ]);
        }
    }

    println!("{table}");
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
    use comfy_table::{Table, Cell, CellAlignment};
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_FULL_CONDENSED);
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
    use comfy_table::Table;
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_FULL_CONDENSED);
    table.set_header(vec!["NAME", "DESCRIPTION", "ENABLED"]);
    for tool in &tools {
        table.add_row(vec![
            tool["name"].as_str().unwrap_or("-"),
            tool["description"].as_str().unwrap_or("-"),
            if tool["enabled"].as_bool().unwrap_or(true) { "yes" } else { "no" },
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

async fn wizard_create_tool(config: &AppConfig) -> Result<()> {
    use inquire::{Confirm, Text};

    println!("{}", "  Creating a new tool...".dimmed());

    let name = Text::new("Tool name:").prompt()?;
    if name.trim().is_empty() {
        return Err(anyhow::anyhow!("Name cannot be empty"));
    }

    let description = Text::new("Description:").prompt()?;

    let handler = Text::new("Handler script path (leave blank to auto-scaffold):").prompt()?;

    let scaffold = if handler.trim().is_empty() {
        Confirm::new("Auto-generate starter script?").with_default(true).prompt()?
    } else {
        false
    };

    let tool = serde_json::json!({
        "name": name.trim(),
        "description": description,
        "parameters": { "type": "object", "properties": {}, "required": [] },
        "handler": if handler.trim().is_empty() { serde_json::Value::Null } else { serde_json::Value::String(handler.trim().to_string()) },
        "scaffold": scaffold,
    });

    match daemon_request(config, DaemonRequest::CreateTool { tool }).await? {
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
        }
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }

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

    match daemon_request(config, DaemonRequest::GetLogs {
        agent_id: selected.to_string(),
        execution_id: None,
        lines: 50,
    }).await? {
        DaemonResponse::ExecutionLog { lines } => {
            if lines.is_empty() {
                println!("{}", "No logs found.".dimmed());
            } else {
                for line in lines {
                    println!("{}", line);
                }
            }
        }
        DaemonResponse::Error { message } => return Err(anyhow::anyhow!("{}", message)),
        _ => {}
    }

    Ok(())
}

// ── Detail printer ────────────────────────────────────────────────────────────

fn print_agent_detail(agent: &serde_json::Value) {
    let fields = [
        ("ID",       "id"),
        ("Name",     "name"),
        ("Model",    "model"),
        ("Provider", "provider"),
        ("Status",   "status"),
        ("Mode",     "execution_mode"),
        ("Schedule", "schedule"),
        ("Memory",   "memory_enabled"),
        ("Deep",     "deep_agent"),
    ];

    println!();
    for (label, key) in &fields {
        let val = match &agent[key] {
            serde_json::Value::Null => "-".to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        println!("  {:12} {}", format!("{label}:").bold(), val);
    }

    if let Some(prompt) = agent["system_prompt"].as_str() {
        let preview: String = prompt.chars().take(120).collect();
        let ellipsis = if prompt.len() > 120 { "…" } else { "" };
        println!("  {:12} {}{}", "Prompt:".bold(), preview, ellipsis);
    }
    println!();
}
