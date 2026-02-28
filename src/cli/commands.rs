use super::{Commands, ToolCommands, ViewCommands};
use crate::core::{
    Agent, AgentStatus, AppConfig, DaemonRequest, DaemonResponse, DeepAgentConfig, ExecutionMode,
    ExecutionResult, ToolDefinition, ToolResource,
};
use anyhow::{anyhow, Context, Result};
use comfy_table::{Cell, CellAlignment, Table};
use owo_colors::OwoColorize;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Send a request to the daemon and return the response
async fn daemon_request(config: &AppConfig, request: DaemonRequest) -> Result<DaemonResponse> {
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

/// Check if daemon is running
fn daemon_socket_exists(config: &AppConfig) -> bool {
    let socket_path = Path::new(&config.socket_path);
    socket_path.exists()
}

async fn is_daemon_running(config: &AppConfig) -> bool {
    matches!(
        daemon_request(config, DaemonRequest::Ping).await,
        Ok(DaemonResponse::Status { running: true, .. })
    )
}

pub async fn handle_command(command: Commands, config: AppConfig) -> Result<()> {
    match command {
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
            tools,
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
            description: new_description,
            temperature: new_temp,
            mode: new_mode,
            schedule: new_schedule,
            tools: new_tools,
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
            if let Some(prompt) = new_prompt {
                agent.system_prompt = prompt;
            }
            if let Some(desc) = new_description {
                agent.description = Some(desc);
            }
            if let Some(temp) = new_temp {
                agent.config.temperature = temp;
            }
            if let Some(mode) = new_mode {
                agent.execution_mode = parse_execution_mode(&mode)?;
            }
            if new_schedule.is_some() {
                agent.schedule = new_schedule;
            }
            if let Some(tools_arg) = new_tools {
                agent.tools = read_tool_definitions(&tools_arg)?;
                if let Some(config) = agent.deep_agent_config.as_mut() {
                    config.available_tools = agent.tools.iter().map(|t| t.name.clone()).collect();
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

        Commands::Import { input, format: _ } => {
            let content = std::fs::read_to_string(&input)?;
            let data: serde_json::Value = serde_json::from_str(&content)?;

            if let Some(agents) = data.get("agents").and_then(|v| v.as_array()) {
                for agent_value in agents {
                    let agent: Agent = serde_json::from_value(agent_value.clone())?;
                    let request = DaemonRequest::CreateAgent {
                        agent: serde_json::to_value(agent)?,
                    };
                    match daemon_request(&config, request).await? {
                        DaemonResponse::Success { message } => println!("{}", message.green()),
                        DaemonResponse::Error { message } => eprintln!("Error: {}", message.red()),
                        _ => {}
                    }
                }
            } else if let Ok(agent) = serde_json::from_value::<Agent>(data.clone()) {
                let request = DaemonRequest::CreateAgent {
                    agent: serde_json::to_value(agent)?,
                };
                match daemon_request(&config, request).await? {
                    DaemonResponse::Success { message } => println!("{}", message.green()),
                    DaemonResponse::Error { message } => return Err(anyhow!("{}", message)),
                    _ => return Err(anyhow!("Unexpected response")),
                }
            } else {
                return Err(anyhow!("Invalid import format"));
            }

            Ok(())
        }

        Commands::Completion { shell: _ } => {
            // TODO: Generate shell completions
            println!("Shell completion generation not yet implemented");
            Ok(())
        }

        Commands::Tool { command } => handle_tool_command(command, &config).await,

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
    }
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
                    let mut table = Table::new();
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
            daemon_stop(&config).await?;
            tokio::time::sleep(Duration::from_secs(2)).await;
            daemon_start(&config, false).await
        }
    }
}

async fn daemon_start(config: &AppConfig, foreground: bool) -> Result<()> {
    if is_daemon_running(config).await {
        println!("Daemon is already running");
        return Ok(());
    }

    // Cleanup stale socket if previous daemon crashed.
    if daemon_socket_exists(config) {
        let _ = std::fs::remove_file(&config.socket_path);
    }

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

        // Start installed daemon binary directly (works outside repo and without cargo in PATH)
        let daemon_bin = resolve_daemon_binary()?;
        let _child = std::process::Command::new(daemon_bin)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        // Wait for daemon to start
        for _ in 0..20 {
            if is_daemon_running(config).await {
                println!("{}", "Daemon started successfully".green());
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        return Err(anyhow!("Failed to start daemon"));
    }

    Ok(())
}

async fn daemon_stop(config: &AppConfig) -> Result<()> {
    if !is_daemon_running(config).await {
        println!("Daemon is not running");
        return Ok(());
    }

    let request = DaemonRequest::Shutdown;
    match daemon_request(config, request).await {
        Ok(DaemonResponse::Success { message }) => {
            println!("{}", message.green());
        }
        Ok(DaemonResponse::Error { message }) => {
            eprintln!("{}", message.red());
        }
        Ok(_) => {
            eprintln!("Unexpected response from daemon");
        }
        Err(_e) => {
            // Daemon might be stuck, clean up socket
            let socket_path = Path::new(&config.socket_path);
            if socket_path.exists() {
                std::fs::remove_file(socket_path)?;
            }
            println!("Daemon stopped (forced)");
        }
    }

    Ok(())
}

async fn handle_tool_command(command: ToolCommands, config: &AppConfig) -> Result<()> {
    match command {
        ToolCommands::Create {
            name,
            description,
            parameters,
            handler,
            scaffold,
        } => {
            let parameters: serde_json::Value = serde_json::from_str(&parameters)
                .map_err(|e| anyhow!("Invalid --parameters JSON: {}", e))?;
            let should_scaffold = scaffold || handler.is_none();
            let resolved_handler = if should_scaffold {
                Some(scaffold_tool_handler(&name, handler.as_deref())?)
            } else {
                handler
            };

            let tool = ToolResource::new(name, description, parameters, resolved_handler);
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
        ToolCommands::Run { id, input, wait } => {
            let input: serde_json::Value = serde_json::from_str(&input)
                .map_err(|e| anyhow!("Invalid --input JSON: {}", e))?;
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

fn print_agent_details(agent: &Agent) {
    let mut table = Table::new();
    table.set_header(vec!["Property", "Value"]);

    table.add_row(vec!["ID", &agent.id]);
    table.add_row(vec!["Name", &agent.name]);
    table.add_row(vec![
        "Description",
        agent.description.as_deref().unwrap_or("N/A"),
    ]);
    table.add_row(vec!["Model", &agent.model]);
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

    let mut table = Table::new();
    table.set_header(vec!["Name", "Model", "Mode", "Status", "Runs", "Last Run"]);

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

        table.add_row(vec![
            Cell::new(agent.name.clone()),
            Cell::new(agent.model.clone()),
            Cell::new(format!("{:?}", agent.execution_mode)),
            Cell::new(status_str),
            Cell::new(format!("{:02}", agent.run_count)).set_alignment(CellAlignment::Right),
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
    let mut table = Table::new();
    table.set_header(vec!["Property", "Value"]);
    table.add_row(vec!["ID", &tool.id]);
    table.add_row(vec!["Name", &tool.name]);
    table.add_row(vec!["Description", &tool.description]);
    table.add_row(vec!["Enabled", if tool.enabled { "true" } else { "false" }]);
    table.add_row(vec!["Handler", tool.handler.as_deref().unwrap_or("N/A")]);
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
    let mut table = Table::new();
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

    let mut table = Table::new();
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
    loop {
        if started.elapsed() > timeout {
            return Err(anyhow!("Timed out waiting for tool execution {}", execution_id));
        }
        let request = DaemonRequest::GetToolExecution {
            id: execution_id.to_string(),
        };
        match daemon_request(config, request).await? {
            DaemonResponse::ToolExecutionResult { result } => {
                let status = result
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_lowercase();
                if status.contains("completed") || status.contains("failed") {
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

fn scaffold_tool_handler(name: &str, handler_arg: Option<&str>) -> Result<String> {
    let path = if let Some(handler) = handler_arg {
        let script_path = handler
            .strip_prefix("/usr/bin/env bash ")
            .unwrap_or(handler)
            .trim();
        std::path::PathBuf::from(script_path)
    } else {
        let cwd = std::env::current_dir()?;
        cwd.join("tools").join(format!("{}.sh", name))
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
