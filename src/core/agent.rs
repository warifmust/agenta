use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;
use uuid::Uuid;

fn default_http_method() -> String {
    "POST".to_string()
}

/// Configuration for an HTTP-backed tool. When present on a tool, the executor
/// makes this request instead of spawning a process: the tool's `handler` field
/// is the URL, the call parameters become the JSON body (for non-GET methods),
/// and `${VAR}` placeholders in the URL and header values are substituted from
/// the tool's allowlisted `secrets`. Kills the curl-in-bash boilerplate for
/// API/webhook tools while keeping the endpoint declared (not model-supplied).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HttpHandler {
    #[serde(default = "default_http_method")]
    pub method: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Classifies a tool's effect on the world. Drives permission/approval guards:
/// read-only tools run freely; write/destructive tools are candidates for a
/// confirmation prompt before execution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    /// No state change — pure compute or read/fetch (calculator, search, read file).
    #[default]
    ReadOnly,
    /// Mutates state or reaches an external system (write file, send message, POST).
    Write,
    /// Irreversible or high-blast-radius (delete, deploy, transfer).
    Destructive,
}

/// Represents a tool that an agent can use
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub handler: Option<String>, // Script command, or the URL when `http` is set
    /// Environment variables (by name) the handler is allowed to receive. The
    /// executor clears the environment and injects only these from agenta's own
    /// env, so a tool can't read secrets it wasn't granted.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Effect classification used for permission/approval guards.
    #[serde(default)]
    pub side_effect: SideEffect,
    /// When set, this is an HTTP tool: the executor calls the endpoint (see
    /// `HttpHandler`) instead of spawning a process. `None` = script handler.
    #[serde(default)]
    pub http: Option<HttpHandler>,
    /// Per-tool execution timeout in seconds. `None` uses the global default
    /// (AGENTA_TOOL_TIMEOUT_SECS or 120s). Long-running orchestrator tools that
    /// drive other agents (e.g. a newsletter pipeline) set this higher so they
    /// aren't killed mid-run.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
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
    /// Env var names this tool is granted (allowlist). See `ToolDefinition::secrets`.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Effect classification. See `SideEffect`.
    #[serde(default)]
    pub side_effect: SideEffect,
    /// HTTP handler config. See `ToolDefinition::http`.
    #[serde(default)]
    pub http: Option<HttpHandler>,
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
            secrets: Vec::new(),
            side_effect: SideEffect::default(),
            http: None,
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
            secrets: self.secrets.clone(),
            side_effect: self.side_effect,
            http: self.http.clone(),
            timeout_secs: None,
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

/// Standalone script managed by CLI/daemon (no LLM involved)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScriptDefinition {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub handler: String,
    pub schedule: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub run_count: u64,
}

impl ScriptDefinition {
    pub fn new(name: String, handler: String, description: Option<String>, schedule: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            handler,
            description,
            schedule,
            enabled: true,
            created_at: now,
            updated_at: now,
            last_run: None,
            run_count: 0,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptExecutionStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ScriptExecution {
    pub id: String,
    pub script_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
    pub stderr: Option<String>,
    pub status: ScriptExecutionStatus,
    pub error: Option<String>,
    pub triggered_by: String,
}

impl ScriptExecution {
    pub fn new(script_id: String, triggered_by: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            script_id,
            started_at: Utc::now(),
            completed_at: None,
            exit_code: None,
            output: None,
            stderr: None,
            status: ScriptExecutionStatus::Pending,
            error: None,
            triggered_by: triggered_by.to_string(),
        }
    }
}

/// Execution mode for the agent
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// One-time execution
    #[default]
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
#[serde(default)]
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
    /// Custom notification message when a sub-agent is spawned.
    /// Supports `{task}` placeholder. Defaults to a generic message if not set.
    /// Example: "🪸 Deploying REEF sub-agent: {task}"
    #[serde(default)]
    pub subagent_spawn_message: Option<String>,
}

impl Default for DeepAgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            enable_reflection: true,
            available_tools: vec![],
            stop_conditions: vec!["task_complete".to_string()],
            allow_sub_agents: false,
            subagent_spawn_message: None,
        }
    }
}

/// Agent status
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
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
    /// Knowledge bases (by name) this agent retrieves from (RAG auto-inject).
    /// Persisted inside the agent's config JSON, so no schema change is needed.
    #[serde(default)]
    pub knowledge_bases: Vec<String>,
    /// Per-agent RAG retrieval top-k (how many passages to inject). Distinct from
    /// the LLM sampling `top_k` above. `None` → fall back to the global `rag_top_k`.
    #[serde(default)]
    pub rag_top_k: Option<usize>,
    /// Whether this agent may run `Destructive` tools autonomously. Off by default:
    /// an irreversible tool won't fire during an unattended run unless explicitly
    /// opted in. (`Write` tools are never gated; only `Destructive`.)
    #[serde(default)]
    pub allow_destructive_tools: bool,
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
            knowledge_bases: vec![],
            rag_top_k: None,
            allow_destructive_tools: false,
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
    #[serde(default = "new_uuid")]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub model: String, // e.g., "llama2", "deepseek/deepseek-chat", etc.
    /// Provider override for this agent. None = use global default_provider from config.
    /// Supported values: "ollama", "openrouter", "deepseek", "openai"
    #[serde(default)]
    pub provider: Option<String>,
    pub system_prompt: String,
    #[serde(default)]
    pub config: AgentConfig,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub execution_mode: ExecutionMode,
    #[serde(default)]
    pub trigger: Option<TriggerType>,
    #[serde(default)]
    pub schedule: Option<String>, // cron expression
    #[serde(default)]
    pub deep_agent_config: Option<DeepAgentConfig>,
    #[serde(default)]
    pub environment: Vec<AgentEnv>,
    #[serde(default)]
    pub memory_enabled: bool,
    #[serde(default)]
    pub is_system: bool,
    #[serde(default)]
    pub status: AgentStatus,
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub last_run: Option<DateTime<Utc>>,
    #[serde(default)]
    pub run_count: u64,
}

fn new_uuid() -> String {
    Uuid::new_v4().to_string()
}

impl Agent {
    pub fn new(name: String, model: String, system_prompt: String) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            description: None,
            model,
            provider: None,
            system_prompt,
            config: AgentConfig::default(),
            tools: vec![],
            execution_mode: ExecutionMode::Once,
            trigger: None,
            schedule: None,
            deep_agent_config: Some(DeepAgentConfig {
                allow_sub_agents: true,
                ..DeepAgentConfig::default()
            }),
            environment: vec![],
            memory_enabled: true,
            is_system: false,
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

    /// The interactive shell builds a minimal agent JSON (no id, partial
    /// deep_agent_config). Deserializing it must fill server-owned fields from
    /// defaults instead of erroring on `enable_reflection`, `id`, etc.
    /// Values are intentionally generic placeholders — only the *shape* matters.
    #[test]
    fn deserializes_partial_agent_from_shell() {
        let only_user_supplied_fields = json!({
            "name": "test-agent",
            "model": "test-model",
            "system_prompt": "test prompt",
            "execution_mode": "once",
            "memory_enabled": true,
            "provider": null,
            "deep_agent_config": { "max_iterations": 10 }
        });
        let agent: Agent = serde_json::from_value(only_user_supplied_fields)
            .expect("partial agent should deserialize");

        // Server-owned fields are filled from defaults, not required in the input.
        assert!(!agent.id.is_empty(), "id should be auto-generated");
        assert_eq!(agent.status, AgentStatus::Draft);
        let deep = agent.deep_agent_config.expect("deep config present");
        assert_eq!(deep.max_iterations, 10, "supplied field is preserved");
        assert!(deep.enable_reflection, "omitted field defaults to true");
    }

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
        // all new agents are deep harness agents with memory by default
        assert!(agent.is_deep_agent());
        assert!(agent.memory_enabled);
        // sub-agent spawning enabled by default
        let dac = agent.deep_agent_config.as_ref().unwrap();
        assert!(dac.allow_sub_agents);
        assert_eq!(dac.max_iterations, 10);
        // provider defaults to None — resolved from config at runtime
        assert!(agent.provider.is_none());
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
