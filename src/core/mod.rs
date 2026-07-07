pub mod agent;
pub mod proposal;
pub mod storage;
pub mod error;
pub mod types;

pub use proposal::{Proposal, ProposalAction, ProposalStatus, Risk};

pub use agent::{
    Agent, AgentConfig, AgentEnv, AgentStatus, DeepAgentConfig, ExecutionMode,
    ExecutionResult, ExecutionStatus, HttpHandler, SideEffect, ToolCall, ToolDefinition,
    ToolExecution, ToolExecutionStatus, ToolResource, TriggerType,
    ScriptDefinition, ScriptExecution, ScriptExecutionStatus,
};
pub use storage::{Storage, SqliteStorage, PostgresStorage};
pub use error::{AgentaError, Result};
pub use types::*;

/// The protected system agent everyone talks to.
pub const MIND_AGENT_NAME: &str = "MIND";

/// MIND's system prompt is compiled into the binary (not read from the per-install
/// DB row) so it ships and upgrades atomically with agenta — a prompt improvement
/// reaches every install on `agenta upgrade`, with zero drift. The DB copy for MIND
/// is vestigial; the runtime executor and the display path both use THIS constant.
/// Edit `src/core/mind_prompt.txt` + rebuild to change MIND everywhere.
pub const MIND_SYSTEM_PROMPT: &str = include_str!("mind_prompt.txt");

/// True when `agent` is the protected MIND system agent (whose prompt is
/// binary-sourced). Used to override its stored prompt at runtime/display.
pub fn is_mind(agent: &agent::Agent) -> bool {
    agent.is_system && agent.name == MIND_AGENT_NAME
}

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
