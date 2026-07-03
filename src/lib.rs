// Many items in this lib are public API consumed by the daemon binary.
// The CLI binary and lib-test targets don't use all of them, so suppress
// false-positive dead_code / unused_imports warnings here.
#![allow(dead_code, unused_imports)]

pub mod cli;
pub mod core;
pub mod knowledge;
pub mod ollama;
pub mod providers;
pub mod scheduler;
pub mod trigger;
pub mod tools;

// Re-export commonly used types
pub use core::{
    Agent, AgentConfig, AgentEnv, AgentStatus, AppConfig, DeepAgentConfig,
    ExecutionMode, ExecutionResult, ExecutionStatus, ToolCall, ToolDefinition,
    TriggerEvent, TriggerType, DaemonRequest, DaemonResponse,
    AgentaError, Result, Storage, SqliteStorage,
};
