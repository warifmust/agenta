pub mod agent;
pub mod storage;
pub mod error;
pub mod types;

pub use agent::{
    Agent, AgentConfig, AgentEnv, AgentStatus, DeepAgentConfig, ExecutionMode,
    ExecutionResult, ExecutionStatus, ToolCall, ToolDefinition, ToolExecution,
    ToolExecutionStatus, ToolResource, TriggerType,
    ScriptDefinition, ScriptExecution, ScriptExecutionStatus,
};
pub use storage::{Storage, SqliteStorage, PostgresStorage};
pub use error::{AgentaError, Result};
pub use types::*;

/// Load `~/.agenta/.env` into the process environment (KEY=VALUE lines; comments
/// and blanks skipped; existing env vars are not overridden). Both the CLI and the
/// daemon call this so `$VAR` references in config.toml resolve for provider keys.
pub fn load_agenta_env() {
    let env_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".agenta")
        .join(".env");

    let Ok(content) = std::fs::read_to_string(&env_path) else {
        return;
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'');
            if !key.is_empty() && std::env::var(key).is_err() {
                std::env::set_var(key, val);
            }
        }
    }
}
