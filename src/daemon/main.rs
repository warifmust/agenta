use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{error, info};

mod state;
mod chat_gateway;
mod rest_api;
use state::DaemonState;
use chat_gateway::start_chat_gateway;
use rest_api::start_rest_api;

use agenta::core::{
    AppConfig, DaemonRequest, DaemonResponse, Storage, SqliteStorage, PostgresStorage,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    info!("Starting agenta daemon...");

    // Load ~/.agenta/.env into process environment (if present)
    load_agenta_env();

    // Load configuration
    let config = AppConfig::load()?;
    config.ensure_dirs()?;

    // Initialize storage
    let storage: Arc<dyn Storage> = if let Some(url) = config
        .database_url
        .as_ref()
        .map(|u| u.trim())
        .filter(|u| !u.is_empty())
    {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            info!("Using Postgres database_url");
            Arc::new(PostgresStorage::new(url).await?)
        } else {
            info!("Using SQLite database_url");
            Arc::new(SqliteStorage::new(url).await?)
        }
    } else {
        info!("Using SQLite database_path");
        Arc::new(SqliteStorage::from_path(PathBuf::from(&config.database_path).as_path()).await?)
    };

    // Create daemon state
    let state = Arc::new(DaemonState::new(storage, &config).await?);

    // Auto-backup all agents on startup
    auto_backup_agents(&state).await;

    // Start background tasks
    let state_clone = state.clone();
    tokio::spawn(async move {
        if let Err(e) = state_clone.start_background_tasks().await {
            error!("Background task error: {}", e);
        }
    });

    // Optional Telegram chat gateway (long polling)
    if let Err(e) = start_chat_gateway(state.clone(), &config).await {
        error!("Chat gateway startup failed: {}", e);
    }

    // Optional REST API + Swagger
    if let Err(e) = start_rest_api(state.clone(), &config).await {
        error!("REST API startup failed: {}", e);
    }

    // Start socket server
    let socket_path = PathBuf::from(&config.socket_path);

    // Remove old socket if exists
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    info!("Daemon listening on socket: {:?}", socket_path);

    // Write PID file
    let pid_file = socket_path.with_extension("pid");
    std::fs::write(&pid_file, std::process::id().to_string())?;

    // Handle signals
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
    let signal_shutdown_tx = shutdown_tx.clone();

    tokio::spawn(async move {
        use signal_hook::consts::signal::*;
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([
            SIGTERM,
            SIGINT,
        ])?;

        if let Some(_sig) = signals.forever().next() {
            let _ = signal_shutdown_tx.send(()).await;
        }

        Ok::<(), anyhow::Error>(())
    });

    info!("Daemon ready. Waiting for connections...");

    // Accept loop
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let shutdown_tx = shutdown_tx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, state, shutdown_tx).await {
                                error!("Connection error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("Accept error: {}", e);
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                info!("Shutdown signal received, stopping daemon...");
                break;
            }
        }
    }

    // Cleanup
    state.stop_all().await;
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pid_file);

    info!("Daemon stopped");

    // Exit promptly. Background tasks (Telegram long-poll loops, scheduler, file
    // watcher) would otherwise keep the tokio runtime alive after we return,
    // leaving the process lingering for many seconds on a graceful shutdown.
    std::process::exit(0);
}

/// Load ~/.agenta/.env into the current process environment.
/// Lines are expected to be KEY=VALUE (comments and blanks are skipped).
fn load_agenta_env() {
    let env_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".agenta")
        .join(".env");

    let Ok(content) = std::fs::read_to_string(&env_path) else {
        return; // .env absent — that's fine
    };

    for line in content.lines() {
        let line = line.trim();
        // Skip blank lines and comments
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'');
            if !key.is_empty() {
                // Don't override values already set in the real environment
                if std::env::var(key).is_err() {
                    std::env::set_var(key, val);
                }
            }
        }
    }
    info!("Loaded env from {:?}", env_path);
}

/// On every daemon startup, export all agents to a timestamped JSON file.
/// Keeps the last 14 backups and silently ignores errors (backup is best-effort).
async fn auto_backup_agents(state: &Arc<DaemonState>) {
    let agents = match state.storage().list_agents().await {
        Ok(a) if !a.is_empty() => a,
        _ => return, // nothing to back up
    };

    let backup_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".agenta")
        .join("exports");

    if std::fs::create_dir_all(&backup_dir).is_err() {
        return;
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let backup_path = backup_dir.join(format!("backup_{}.json", timestamp));

    let data = serde_json::json!({ "agents": agents, "backup_at": timestamp });
    if let Ok(json) = serde_json::to_string_pretty(&data) {
        let _ = std::fs::write(&backup_path, json);
        info!("Auto-backup saved: {:?} ({} agents)", backup_path, agents.len());
    }

    // Prune: keep only the 14 most recent backups
    if let Ok(entries) = std::fs::read_dir(&backup_dir) {
        let mut backups: Vec<_> = entries
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("backup_"))
            .collect();
        backups.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
        for old in backups.into_iter().skip(14) {
            let _ = std::fs::remove_file(old.path());
        }
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    state: Arc<DaemonState>,
    shutdown_tx: mpsc::Sender<()>,
) -> anyhow::Result<()> {
    // Read the full request. The client writes the request then shuts down its
    // write half, so read_to_end returns the complete message regardless of size
    // (a single fixed-size read truncated large requests, e.g. big agent updates).
    let mut buffer = Vec::new();
    stream.read_to_end(&mut buffer).await?;

    // Parse request
    let request: DaemonRequest = match serde_json::from_slice(&buffer) {
        Ok(req) => req,
        Err(e) => {
            let response = DaemonResponse::Error {
                message: format!("Failed to parse request: {}", e),
            };
            let response_bytes = serde_json::to_vec(&response)?;
            stream.write_all(&response_bytes).await?;
            return Ok(());
        }
    };

    // Process request
    let response = process_request(request, state, shutdown_tx).await;

    // Send response
    let response_bytes = serde_json::to_vec(&response)?;
    stream.write_all(&response_bytes).await?;

    Ok(())
}

async fn process_request(
    request: DaemonRequest,
    state: Arc<DaemonState>,
    shutdown_tx: mpsc::Sender<()>,
) -> DaemonResponse {
    match request {
        DaemonRequest::CreateAgent { agent } => {
            match serde_json::from_value(agent) {
                Ok(agent) => {
                    match state.create_agent(agent).await {
                        Ok(id) => DaemonResponse::Success {
                            message: format!("Agent created: {}", id),
                        },
                        Err(e) => DaemonResponse::Error {
                            message: e.to_string(),
                        },
                    }
                }
                Err(e) => DaemonResponse::Error {
                    message: format!("Invalid agent data: {}", e),
                },
            }
        }

        DaemonRequest::UpdateAgent { id, agent } => {
            match serde_json::from_value(agent) {
                Ok(agent) => {
                    match state.update_agent(id, agent).await {
                        Ok(_) => DaemonResponse::Success {
                            message: "Agent updated".to_string(),
                        },
                        Err(e) => DaemonResponse::Error {
                            message: e.to_string(),
                        },
                    }
                }
                Err(e) => DaemonResponse::Error {
                    message: format!("Invalid agent data: {}", e),
                },
            }
        }

        DaemonRequest::DeleteAgent { id } => {
            match state.delete_agent(&id).await {
                Ok(true) => DaemonResponse::Success {
                    message: "Agent deleted".to_string(),
                },
                Ok(false) => DaemonResponse::Error {
                    message: "Agent not found".to_string(),
                },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::GetAgent { id } => {
            match state.get_agent(&id).await {
                Ok(Some(agent)) => {
                    match serde_json::to_value(agent) {
                        Ok(value) => DaemonResponse::AgentDetails { agent: value },
                        Err(e) => DaemonResponse::Error {
                            message: e.to_string(),
                        },
                    }
                }
                Ok(None) => DaemonResponse::Error {
                    message: "Agent not found".to_string(),
                },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::ListAgents => {
            match state.list_agents().await {
                Ok(agents) => {
                    let values: Vec<serde_json::Value> = agents
                        .into_iter()
                        .filter_map(|a| serde_json::to_value(a).ok())
                        .collect();
                    DaemonResponse::AgentList { agents: values }
                }
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::RunAgent { id, input } => {
            match state.run_agent(&id, input).await {
                Ok(execution_id) => DaemonResponse::ExecutionStarted { execution_id },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::StopAgent { id } => {
            match state.stop_agent(&id).await {
                Ok(_) => DaemonResponse::Success {
                    message: "Agent stopped".to_string(),
                },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::GetLogs { agent_id, execution_id, lines } => {
            match state.get_logs(&agent_id, execution_id.as_deref(), lines).await {
                Ok(logs) => DaemonResponse::ExecutionLog { lines: logs },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::GetExecution { id } => {
            match state.get_execution(&id).await {
                Ok(Some(execution)) => match serde_json::to_value(execution) {
                    Ok(value) => DaemonResponse::ExecutionResult { result: value },
                    Err(e) => DaemonResponse::Error {
                        message: e.to_string(),
                    },
                },
                Ok(None) => DaemonResponse::Error {
                    message: "Execution not found".to_string(),
                },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::ListExecutions { limit } => {
            match state.list_executions(limit as i64).await {
                Ok(executions) => {
                    let values: Vec<serde_json::Value> = executions
                        .into_iter()
                        .filter_map(|e| serde_json::to_value(e).ok())
                        .collect();
                    DaemonResponse::ExecutionList { executions: values }
                }
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::CreateTool { tool } => {
            match serde_json::from_value(tool) {
                Ok(tool) => match state.create_tool(tool).await {
                    Ok(id) => DaemonResponse::Success {
                        message: format!("Tool created: {}", id),
                    },
                    Err(e) => DaemonResponse::Error {
                        message: e.to_string(),
                    },
                },
                Err(e) => DaemonResponse::Error {
                    message: format!("Invalid tool data: {}", e),
                },
            }
        }

        DaemonRequest::UpdateTool { id, tool } => {
            match serde_json::from_value(tool) {
                Ok(tool) => match state.update_tool(&id, tool).await {
                    Ok(_) => DaemonResponse::Success {
                        message: "Tool updated".to_string(),
                    },
                    Err(e) => DaemonResponse::Error {
                        message: e.to_string(),
                    },
                },
                Err(e) => DaemonResponse::Error {
                    message: format!("Invalid tool data: {}", e),
                },
            }
        }

        DaemonRequest::DeleteTool { id } => {
            match state.delete_tool(&id).await {
                Ok(true) => DaemonResponse::Success {
                    message: "Tool deleted".to_string(),
                },
                Ok(false) => DaemonResponse::Error {
                    message: "Tool not found".to_string(),
                },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::GetTool { id } => {
            match state.get_tool(&id).await {
                Ok(Some(tool)) => match serde_json::to_value(tool) {
                    Ok(value) => DaemonResponse::ToolDetails { tool: value },
                    Err(e) => DaemonResponse::Error {
                        message: e.to_string(),
                    },
                },
                Ok(None) => DaemonResponse::Error {
                    message: "Tool not found".to_string(),
                },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            }
        }

        DaemonRequest::ListTools => match state.list_tools().await {
            Ok(tools) => {
                let values: Vec<serde_json::Value> = tools
                    .into_iter()
                    .filter_map(|t| serde_json::to_value(t).ok())
                    .collect();
                DaemonResponse::ToolList { tools: values }
            }
            Err(e) => DaemonResponse::Error {
                message: e.to_string(),
            },
        },

        DaemonRequest::RunTool { id, input } => match state.run_tool(&id, input).await {
            Ok(execution_id) => DaemonResponse::ToolExecutionStarted { execution_id },
            Err(e) => DaemonResponse::Error {
                message: e.to_string(),
            },
        },

        DaemonRequest::GetToolExecution { id } => match state.get_tool_execution(&id).await {
            Ok(Some(result)) => match serde_json::to_value(result) {
                Ok(value) => DaemonResponse::ToolExecutionResult { result: value },
                Err(e) => DaemonResponse::Error {
                    message: e.to_string(),
                },
            },
            Ok(None) => DaemonResponse::Error {
                message: "Tool execution not found".to_string(),
            },
            Err(e) => DaemonResponse::Error {
                message: e.to_string(),
            },
        },

        DaemonRequest::GetToolLogs {
            tool_id,
            execution_id,
            lines,
        } => match state
            .get_tool_logs(&tool_id, execution_id.as_deref(), lines)
            .await
        {
            Ok(logs) => DaemonResponse::ToolExecutionLog { lines: logs },
            Err(e) => DaemonResponse::Error {
                message: e.to_string(),
            },
        },

        DaemonRequest::CreateScript { script } => {
            use agenta::core::ScriptDefinition;
            match serde_json::from_value::<ScriptDefinition>(script) {
                Ok(script) => match state.create_script(script).await {
                    Ok(id) => DaemonResponse::Success {
                        message: format!("Script created: {}", id),
                    },
                    Err(e) => DaemonResponse::Error { message: e.to_string() },
                },
                Err(e) => DaemonResponse::Error {
                    message: format!("Invalid script data: {}", e),
                },
            }
        }

        DaemonRequest::UpdateScript { id, script } => {
            use agenta::core::ScriptDefinition;
            match serde_json::from_value::<ScriptDefinition>(script) {
                Ok(script) => match state.update_script(&id, script).await {
                    Ok(_) => DaemonResponse::Success { message: "Script updated".to_string() },
                    Err(e) => DaemonResponse::Error { message: e.to_string() },
                },
                Err(e) => DaemonResponse::Error {
                    message: format!("Invalid script data: {}", e),
                },
            }
        }

        DaemonRequest::DeleteScript { id } => match state.delete_script(&id).await {
            Ok(true) => DaemonResponse::Success { message: "Script deleted".to_string() },
            Ok(false) => DaemonResponse::Error { message: "Script not found".to_string() },
            Err(e) => DaemonResponse::Error { message: e.to_string() },
        },

        DaemonRequest::GetScript { id } => match state.get_script(&id).await {
            Ok(Some(script)) => match serde_json::to_value(script) {
                Ok(value) => DaemonResponse::ScriptDetails { script: value },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            },
            Ok(None) => DaemonResponse::Error { message: "Script not found".to_string() },
            Err(e) => DaemonResponse::Error { message: e.to_string() },
        },

        DaemonRequest::ListScripts => match state.list_scripts().await {
            Ok(scripts) => {
                let values: Vec<serde_json::Value> = scripts
                    .into_iter()
                    .filter_map(|s| serde_json::to_value(s).ok())
                    .collect();
                DaemonResponse::ScriptList { scripts: values }
            }
            Err(e) => DaemonResponse::Error { message: e.to_string() },
        },

        DaemonRequest::RunScript { id } => match state.run_script(&id).await {
            Ok(execution_id) => DaemonResponse::ScriptExecutionStarted { execution_id },
            Err(e) => DaemonResponse::Error { message: e.to_string() },
        },

        DaemonRequest::GetScriptLogs { script_id, execution_id, lines } => {
            match state.get_script_logs(&script_id, execution_id.as_deref(), lines).await {
                Ok(logs) => DaemonResponse::ScriptExecutionLog { lines: logs },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::Ping => {
            DaemonResponse::Status {
                running: true,
                pid: Some(std::process::id()),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }
        }

        DaemonRequest::Shutdown => {
            let _ = shutdown_tx.send(()).await;
            DaemonResponse::Success {
                message: "Shutting down...".to_string(),
            }
        }
    }
}
