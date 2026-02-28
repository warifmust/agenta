use thiserror::Error;

pub type Result<T> = std::result::Result<T, AgentaError>;

#[derive(Error, Debug)]
pub enum AgentaError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Agent not found: {0}")]
    AgentNotFound(String),

    #[error("Agent already exists: {0}")]
    AgentAlreadyExists(String),

    #[error("Ollama error: {0}")]
    Ollama(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Trigger error: {0}")]
    Trigger(String),

    #[error("Socket error: {0}")]
    Socket(String),

    #[error("Daemon not running")]
    DaemonNotRunning,

    #[error("Invalid cron expression: {0}")]
    InvalidCron(String),

    #[error("Tool execution error: {0}")]
    ToolExecution(String),

    #[error("Deep agent limit exceeded")]
    DeepAgentLimitExceeded,

    #[error("Unknown error: {0}")]
    Unknown(String),
}
