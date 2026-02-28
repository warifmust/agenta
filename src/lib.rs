pub mod cli;
pub mod core;
pub mod ollama;
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
