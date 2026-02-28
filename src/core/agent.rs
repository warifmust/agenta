use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Represents a tool that an agent can use
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub handler: Option<String>, // Path to script or built-in command
}

/// First-class tool resource managed by CLI/daemon
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ToolResource {
    pub id: String,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub handler: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ToolResource {
    pub fn new(
        name: String,
        description: String,
        parameters: serde_json::Value,
        handler: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description,
            parameters,
            handler,
            enabled: true,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn as_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
            handler: self.handler.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ToolExecution {
    pub id: String,
    pub tool_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub input: serde_json::Value,
    pub output: Option<String>,
    pub status: ToolExecutionStatus,
    pub error: Option<String>,
}

impl ToolExecution {
    pub fn new(tool_id: String, input: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            tool_id,
            started_at: Utc::now(),
            completed_at: None,
            input,
            output: None,
            status: ToolExecutionStatus::Pending,
            error: None,
        }
    }
}

/// Execution mode for the agent
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// One-time execution
    Once,
    /// Scheduled execution (cron-like)
    Scheduled,
    /// Event-triggered execution
    Triggered,
    /// Always running (reactive agent)
    Continuous,
}

/// Type of trigger for event-based execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TriggerType {
    /// File system watcher
    FileWatcher {
        path: String,
        events: Vec<String>, // create, modify, delete
        pattern: Option<String>, // regex pattern
    },
    /// HTTP webhook (local only for now)
    HttpWebhook {
        port: u16,
        path: String,
        method: String,
    },
    /// Command output trigger
    CommandTrigger {
        command: String,
        interval_seconds: u64,
        condition: String, // expected output condition
    },
    /// Time-based trigger (at specific time)
    TimeTrigger {
        datetime: DateTime<Utc>,
    },
}

/// Deep agent configuration for multi-step reasoning
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeepAgentConfig {
    /// Maximum number of iterations
    pub max_iterations: u32,
    /// Whether to enable reflection on responses
    pub enable_reflection: bool,
    /// Tools available to the deep agent
    pub available_tools: Vec<String>,
    /// Stopping conditions
    pub stop_conditions: Vec<String>,
    /// Whether to allow agent to spawn sub-agents
    pub allow_sub_agents: bool,
}

impl Default for DeepAgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            enable_reflection: true,
            available_tools: vec![],
            stop_conditions: vec!["task_complete".to_string()],
            allow_sub_agents: false,
        }
    }
}

/// Agent status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Draft,
    Active,
    Paused,
    Running,
    Error,
}

/// Agent configuration
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentConfig {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub max_tokens: u32,
    pub context_window: u32,
    pub stop_sequences: Vec<String>,
    pub seed: Option<u64>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.9,
            top_k: 40,
            max_tokens: 2048,
            context_window: 4096,
            stop_sequences: vec![],
            seed: None,
        }
    }
}

/// Environment variables for the agent
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentEnv {
    pub key: String,
    pub value: String,
    #[serde(default)]
    pub is_secret: bool,
}

/// An AI Agent definition
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub model: String, // e.g., "llama2", "mistral", etc.
    pub system_prompt: String,
    pub config: AgentConfig,
    pub tools: Vec<ToolDefinition>,
    pub execution_mode: ExecutionMode,
    pub trigger: Option<TriggerType>,
    pub schedule: Option<String>, // cron expression
    pub deep_agent_config: Option<DeepAgentConfig>,
    pub environment: Vec<AgentEnv>,
    pub status: AgentStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub run_count: u64,
}

impl Agent {
    pub fn new(name: String, model: String, system_prompt: String) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description: None,
            model,
            system_prompt,
            config: AgentConfig::default(),
            tools: vec![],
            execution_mode: ExecutionMode::Once,
            trigger: None,
            schedule: None,
            deep_agent_config: None,
            environment: vec![],
            status: AgentStatus::Draft,
            created_at: now,
            updated_at: now,
            last_run: None,
            run_count: 0,
        }
    }

    pub fn is_deep_agent(&self) -> bool {
        self.deep_agent_config.is_some()
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

/// Execution result
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ExecutionResult {
    pub id: String,
    pub agent_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub input: String,
    pub output: Option<String>,
    pub status: ExecutionStatus,
    pub iterations: u32,
    pub tool_calls: Vec<ToolCall>,
    pub error: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ToolCall {
    pub tool_name: String,
    pub parameters: serde_json::Value,
    pub result: String,
    pub timestamp: DateTime<Utc>,
}

impl ExecutionResult {
    pub fn new(agent_id: String, input: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            agent_id,
            started_at: Utc::now(),
            completed_at: None,
            input,
            output: None,
            status: ExecutionStatus::Pending,
            iterations: 0,
            tool_calls: vec![],
            error: None,
            metadata: serde_json::json!({}),
        }
    }

    pub fn new_with_id(agent_id: String, input: String, id: String) -> Self {
        let mut execution = Self::new(agent_id, input);
        execution.id = id;
        execution
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_new_sets_expected_defaults() {
        let agent = Agent::new(
            "travel-guide".to_string(),
            "qwen3:latest".to_string(),
            "You are helpful".to_string(),
        );

        assert_eq!(agent.name, "travel-guide");
        assert_eq!(agent.model, "qwen3:latest");
        assert_eq!(agent.execution_mode, ExecutionMode::Once);
        assert_eq!(agent.status, AgentStatus::Draft);
        assert_eq!(agent.run_count, 0);
        assert!(!agent.id.is_empty());
        assert!(!agent.is_deep_agent());
    }

    #[test]
    fn agent_touch_updates_timestamp() {
        let mut agent = Agent::new(
            "planner".to_string(),
            "model".to_string(),
            "prompt".to_string(),
        );
        let before = agent.updated_at;
        std::thread::sleep(std::time::Duration::from_millis(1));
        agent.touch();
        assert!(agent.updated_at > before);
    }

    #[test]
    fn tool_resource_can_convert_to_definition() {
        let resource = ToolResource::new(
            "maps_route".to_string(),
            "Route planning".to_string(),
            json!({"type":"object"}),
            Some("/usr/bin/env bash tools/maps_route.sh".to_string()),
        );

        let def = resource.as_definition();
        assert_eq!(def.name, "maps_route");
        assert_eq!(def.description, "Route planning");
        assert_eq!(def.parameters, json!({"type":"object"}));
        assert_eq!(
            def.handler.as_deref(),
            Some("/usr/bin/env bash tools/maps_route.sh")
        );
    }

    #[test]
    fn execution_result_new_with_id_overrides_id() {
        let exec = ExecutionResult::new_with_id(
            "agent-1".to_string(),
            "hello".to_string(),
            "exec-123".to_string(),
        );
        assert_eq!(exec.id, "exec-123");
        assert_eq!(exec.agent_id, "agent-1");
        assert_eq!(exec.input, "hello");
        assert_eq!(exec.status, ExecutionStatus::Pending);
    }
}
