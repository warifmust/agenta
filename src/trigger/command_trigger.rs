use regex::Regex;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{mpsc, RwLock};
use tokio::time::interval;
use tracing::{error, info, warn};

use crate::core::{AgentaError, Result, TriggerEvent};

pub struct CommandTrigger {
    running: Arc<RwLock<bool>>,
}

impl CommandTrigger {
    pub fn new() -> Self {
        Self {
            running: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn start_monitoring(
        &self,
        agent_id: String,
        command: String,
        condition: String,
        interval_seconds: u64,
        event_sender: mpsc::Sender<TriggerEvent>,
    ) -> Result<()> {
        let running = self.running.clone();
        *running.write().await = true;

        let condition_regex = Regex::new(&condition)
            .map_err(|e| AgentaError::Trigger(format!("Invalid condition regex: {}", e)))?;

        let command_clone = command.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(interval_seconds));

            while *running.read().await {
                ticker.tick().await;

                match Self::run_command(&command_clone).await {
                    Ok(output) => {
                        let matched = condition_regex.is_match(&output);
                        if matched {
                            let event = TriggerEvent::CommandOutput {
                                agent_id: agent_id.clone(),
                                command: command_clone.clone(),
                                output: output.clone(),
                                matched: true,
                            };

                            if let Err(e) = event_sender.send(event).await {
                                error!("Failed to send command trigger event: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Command trigger failed: {}", e);
                    }
                }
            }
        });

        info!("Started command trigger: {}", command);
        Ok(())
    }

    async fn run_command(command: &str) -> Result<String> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return Err(AgentaError::Trigger("Empty command".to_string()));
        }

        let program = parts[0];
        let args = &parts[1..];

        let output = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
            .map_err(|e| AgentaError::Execution(format!("Failed to run command: {}", e)))?;

        if !output.status.success() {
            return Err(AgentaError::Execution(format!(
                "Command exited with code: {:?}",
                output.status.code()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    }

    pub async fn stop(&self) -> Result<()> {
        *self.running.write().await = false;
        info!("Stopped command trigger");
        Ok(())
    }
}

impl Default for CommandTrigger {
    fn default() -> Self {
        Self::new()
    }
}
