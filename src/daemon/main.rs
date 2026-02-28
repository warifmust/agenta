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
    let state = Arc::new(DaemonState::new(storage, config.ollama_url.clone()).await?);

    // Start background tasks
    let state_clone = state.clone();
    tokio::spawn(async move {
        if let Err(e) = state_clone.start_background_tasks().await {
            error!("Background task error: {}", e);
        }
    });

    // Optional Telegram/WhatsApp chat bridge
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

    tokio::spawn(async move {
        use signal_hook::consts::signal::*;
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([
            SIGTERM,
            SIGINT,
        ])?;

        if let Some(_sig) = signals.forever().next() {
            let _ = shutdown_tx.send(()).await;
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
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, state).await {
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
    Ok(())
}

async fn handle_connection(
    mut stream: UnixStream,
    state: Arc<DaemonState>,
) -> anyhow::Result<()> {
    // Read request
    let mut buffer = vec![0u8; 8192];
    let n = stream.read(&mut buffer).await?;
    buffer.truncate(n);

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
    let response = process_request(request, state).await;

    // Send response
    let response_bytes = serde_json::to_vec(&response)?;
    stream.write_all(&response_bytes).await?;

    Ok(())
}

async fn process_request(
    request: DaemonRequest,
    state: Arc<DaemonState>,
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

        DaemonRequest::Ping => {
            DaemonResponse::Status {
                running: true,
                pid: Some(std::process::id()),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }
        }

        DaemonRequest::Shutdown => {
            // Signal shutdown - handled by main loop
            DaemonResponse::Success {
                message: "Shutting down...".to_string(),
            }
        }
    }
}
