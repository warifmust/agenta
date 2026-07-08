use serde::{Deserialize, Serialize};

/// Response from daemon to CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum DaemonResponse {
    Success { message: String },
    Error { message: String },
    AgentList { agents: Vec<serde_json::Value> },
    AgentDetails { agent: serde_json::Value },
    ExecutionStarted { execution_id: String },
    ExecutionResult { result: serde_json::Value },
    ExecutionList { executions: Vec<serde_json::Value> },
    ExecutionLog { lines: Vec<String> },
    ToolList { tools: Vec<serde_json::Value> },
    ToolDetails { tool: serde_json::Value },
    ToolExecutionStarted { execution_id: String },
    ToolExecutionResult { result: serde_json::Value },
    ToolExecutionLog { lines: Vec<String> },
    ScriptList { scripts: Vec<serde_json::Value> },
    ScriptDetails { script: serde_json::Value },
    ScriptExecutionStarted { execution_id: String },
    ScriptExecutionLog { lines: Vec<String> },
    ProposalList { proposals: Vec<serde_json::Value> },
    ProposalDetails { proposal: serde_json::Value },
    MemoryList { memories: Vec<serde_json::Value> },
    Status { running: bool, pid: Option<u32>, version: String },
}

/// Request from CLI to daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum DaemonRequest {
    CreateAgent { agent: serde_json::Value },
    UpdateAgent { id: String, agent: serde_json::Value },
    DeleteAgent { id: String },
    GetAgent { id: String },
    ListAgents,
    RunAgent { id: String, input: Option<String> },
    StopAgent { id: String },
    GetLogs { agent_id: String, execution_id: Option<String>, lines: usize },
    GetExecution { id: String },
    ListExecutions { limit: usize },
    CreateTool { tool: serde_json::Value },
    UpdateTool { id: String, tool: serde_json::Value },
    DeleteTool { id: String },
    GetTool { id: String },
    ListTools,
    RunTool { id: String, input: serde_json::Value },
    GetToolExecution { id: String },
    GetToolLogs { tool_id: String, execution_id: Option<String>, lines: usize },
    CreateScript { script: serde_json::Value },
    UpdateScript { id: String, script: serde_json::Value },
    DeleteScript { id: String },
    GetScript { id: String },
    ListScripts,
    RunScript { id: String },
    GetScriptLogs { script_id: String, execution_id: Option<String>, lines: usize },
    ListProposals { status: Option<String> },
    GetProposal { id: String },
    ApproveProposal { id: String },
    RejectProposal { id: String, reason: Option<String> },
    ListMemories { scope: String, active_only: bool },
    AddMemory { scope: String, kind: String, content: String },
    DeleteMemory { id: String },
    Ping,
    Shutdown,
}

/// Socket message wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketMessage {
    pub id: String,
    pub payload: DaemonRequest,
}

/// Trigger event from file watcher or other triggers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TriggerEvent {
    FileCreated { path: String },
    FileModified { path: String },
    FileDeleted { path: String },
    HttpRequest { agent_id: String, method: String, path: String, body: Option<String> },
    CommandOutput { agent_id: String, command: String, output: String, matched: bool },
    Scheduled { agent_id: String, cron: String },
    ScriptScheduled { script_id: String, cron: String },
    Manual { input: String },
}

/// A single Telegram bot entry for multi-bot polling support
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramBotConfig {
    /// Bot token (or env var name like "$CORAL_BOT_TOKEN")
    pub token: String,
    /// Which agent handles messages from this bot
    pub default_agent: String,
    /// Friendly label shown in logs
    #[serde(default)]
    pub name: Option<String>,
}

/// Per-provider configuration block (URL + API key).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    /// Base URL override (optional — each provider has a sensible default).
    #[serde(default)]
    pub url: Option<String>,
    /// API key or "$ENV_VAR" reference.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Configuration for the agenta application
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Kept for backward compatibility — used as the Ollama URL when
    /// no explicit [providers.ollama] block is present.
    pub ollama_url: String,
    pub database_path: String,
    #[serde(default)]
    pub database_url: Option<String>,
    pub socket_path: String,
    pub log_level: String,
    pub default_model: String,
    /// Default provider used when an agent has no explicit provider set.
    /// Supported values: "ollama" (default), "openrouter", "deepseek", "openai"
    #[serde(default = "default_provider")]
    pub default_provider: Option<String>,
    /// Per-provider config blocks: [providers.ollama], [providers.deepseek], etc.
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderConfig>,
    #[serde(default = "default_chat_gateway_port")]
    pub chat_gateway_port: u16,
    /// Legacy single-bot config (still supported for backward compat)
    #[serde(default)]
    pub telegram_bot_token: Option<String>,
    #[serde(default)]
    pub telegram_default_agent: Option<String>,
    /// Multi-bot config — each entry gets its own polling loop
    #[serde(default)]
    pub telegram_bots: Vec<TelegramBotConfig>,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    #[serde(default)]
    pub api_token: Option<String>,
    /// Timezone for cron scheduling (e.g. "Asia/Kuala_Lumpur").
    /// Defaults to system local timezone if not set.
    #[serde(default)]
    pub timezone: Option<String>,
    /// GitHub owner of the agenta-tools registry (default: agenta-tools)
    #[serde(default = "default_registry_owner")]
    pub registry_owner: String,
    /// GitHub repo of the agenta-tools registry (default: agenta-tools)
    #[serde(default = "default_registry_repo")]
    pub registry_repo: String,
    /// Number of knowledge passages to retrieve and inject per RAG query (top-k).
    /// Higher = more recall/context (and tokens); lower = tighter/cheaper.
    #[serde(default = "default_rag_top_k")]
    pub rag_top_k: usize,
}

fn default_registry_owner() -> String { "agenta-tools".to_string() }
fn default_registry_repo() -> String { "agenta-tools".to_string() }
fn default_rag_top_k() -> usize { 8 }

fn default_provider() -> Option<String> {
    Some("ollama".to_string())
}

fn default_chat_gateway_port() -> u16 {
    8790
}

fn default_api_port() -> u16 {
    8789
}

impl Default for AppConfig {
    fn default() -> Self {
        let data_dir = dirs::data_dir()
            .map(|d| d.join("agenta"))
            .unwrap_or_else(|| std::path::PathBuf::from(".agenta"));

        Self {
            ollama_url: "http://localhost:11434".to_string(),
            database_path: data_dir.join("agenta.db").to_string_lossy().to_string(),
            database_url: None,
            socket_path: data_dir.join("agenta.sock").to_string_lossy().to_string(),
            log_level: "info".to_string(),
            default_model: "llama2".to_string(),
            default_provider: Some("ollama".to_string()),
            providers: std::collections::HashMap::new(),
            chat_gateway_port: 8790,
            telegram_bot_token: None,
            telegram_default_agent: None,
            telegram_bots: Vec::new(),
            api_port: 8789,
            api_token: None,
            timezone: None,
            registry_owner: default_registry_owner(),
            registry_repo: default_registry_repo(),
            rag_top_k: default_rag_top_k(),
        }
    }
}

impl AppConfig {
    /// Resolve provider URL: check [providers.<name>].url, fall back to ollama_url for ollama.
    pub fn provider_url(&self, provider: &str) -> Option<String> {
        self.providers
            .get(provider)
            .and_then(|p| p.url.clone())
            .or_else(|| {
                if provider == "ollama" {
                    Some(self.ollama_url.clone())
                } else {
                    None
                }
            })
    }

    /// Resolve provider API key: check [providers.<name>].api_key, expand $ENV_VAR if needed.
    pub fn provider_api_key(&self, provider: &str) -> Option<String> {
        let raw = self.providers.get(provider)?.api_key.as_ref()?;
        if let Some(var_name) = raw.strip_prefix('$') {
            std::env::var(var_name).ok()
        } else {
            Some(raw.clone())
        }
    }
}

impl AppConfig {
    pub fn load() -> anyhow::Result<Self> {
        // Use ~/.agenta for all config — same directory as .env, tools, scripts
        let config_dir = dirs::home_dir()
            .map(|d| d.join(".agenta"))
            .unwrap_or_else(|| std::path::PathBuf::from(".agenta"));

        let config_file = config_dir.join("config.toml");

        if config_file.exists() {
            let content = std::fs::read_to_string(&config_file)?;
            let config: AppConfig = toml::from_str(&content)?;
            Ok(config)
        } else {
            let config = AppConfig::default();
            std::fs::create_dir_all(&config_dir)?;
            let content = toml::to_string_pretty(&config)?;
            std::fs::write(&config_file, content)?;
            Ok(config)
        }
    }

    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        if let Some(url) = &self.database_url {
            if url.starts_with("postgres://") || url.starts_with("postgresql://") {
                return Ok(());
            }
        }

        let data_dir = std::path::Path::new(&self.database_path)
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(data_dir)?;
        Ok(())
    }
}

/// Tool execution request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionRequest {
    pub tool_name: String,
    pub parameters: serde_json::Value,
}

/// Tool execution response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResponse {
    pub success: bool,
    pub result: String,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    #[test]
    fn app_config_default_has_expected_ports() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.chat_gateway_port, 8790);
        assert_eq!(cfg.api_port, 8789);
        assert_eq!(cfg.log_level, "info");
    }

    #[test]
    fn ensure_dirs_creates_sqlite_parent_dir() {
        let base = std::env::temp_dir().join(format!("agenta-test-{}", Uuid::new_v4()));
        let db_path = base.join("data").join("agenta.db");
        let cfg = AppConfig {
            database_path: db_path.to_string_lossy().to_string(),
            ..AppConfig::default()
        };

        cfg.ensure_dirs().expect("ensure_dirs should create parent dir");
        assert!(db_path.parent().unwrap().exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn ensure_dirs_skips_creation_for_postgres() {
        let base = std::env::temp_dir().join(format!("agenta-test-{}", Uuid::new_v4()));
        let db_path = base.join("nested").join("agenta.db");
        let cfg = AppConfig {
            database_path: db_path.to_string_lossy().to_string(),
            database_url: Some("postgres://user:pass@localhost:5432/db".to_string()),
            ..AppConfig::default()
        };

        cfg.ensure_dirs().expect("postgres path should be ignored");
        assert!(!PathBuf::from(&cfg.database_path)
            .parent()
            .unwrap()
            .exists());
    }

    #[test]
    fn daemon_request_serde_uses_snake_case_variant_names() {
        let req = DaemonRequest::ListExecutions { limit: 50 };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(json.contains("\"type\":\"list_executions\""));
        assert!(json.contains("\"limit\":50"));
    }

    // ── provider / timezone fields ────────────────────────────────────────────

    #[test]
    fn app_config_default_provider_is_ollama() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.default_provider.as_deref(), Some("ollama"));
    }

    #[test]
    fn app_config_default_timezone_is_none() {
        let cfg = AppConfig::default();
        assert!(cfg.timezone.is_none());
    }

    #[test]
    fn provider_url_returns_ollama_url_for_ollama_provider() {
        let cfg = AppConfig {
            ollama_url: "http://localhost:11434".to_string(),
            ..AppConfig::default()
        };
        assert_eq!(
            cfg.provider_url("ollama").as_deref(),
            Some("http://localhost:11434")
        );
    }

    #[test]
    fn provider_url_returns_custom_url_from_providers_map() {
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "deepseek".to_string(),
            ProviderConfig {
                url: Some("https://api.deepseek.com/v1".to_string()),
                api_key: None,
            },
        );
        assert_eq!(
            cfg.provider_url("deepseek").as_deref(),
            Some("https://api.deepseek.com/v1")
        );
    }

    #[test]
    fn provider_url_returns_none_for_unknown_provider_with_no_entry() {
        let cfg = AppConfig::default();
        assert!(cfg.provider_url("openrouter").is_none());
    }

    #[test]
    fn provider_api_key_returns_literal_value() {
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "openai".to_string(),
            ProviderConfig {
                url: None,
                api_key: Some("sk-literal-key".to_string()),
            },
        );
        assert_eq!(
            cfg.provider_api_key("openai").as_deref(),
            Some("sk-literal-key")
        );
    }

    #[test]
    fn provider_api_key_expands_env_var() {
        std::env::set_var("AGENTA_TEST_DEEPSEEK_KEY", "sk-from-env");
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "deepseek".to_string(),
            ProviderConfig {
                url: None,
                api_key: Some("$AGENTA_TEST_DEEPSEEK_KEY".to_string()),
            },
        );
        assert_eq!(
            cfg.provider_api_key("deepseek").as_deref(),
            Some("sk-from-env")
        );
        std::env::remove_var("AGENTA_TEST_DEEPSEEK_KEY");
    }

    #[test]
    fn provider_api_key_returns_none_when_env_var_not_set() {
        std::env::remove_var("AGENTA_TEST_MISSING_KEY");
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                url: None,
                api_key: Some("$AGENTA_TEST_MISSING_KEY".to_string()),
            },
        );
        assert!(cfg.provider_api_key("openrouter").is_none());
    }

    #[test]
    fn provider_api_key_returns_none_for_unknown_provider() {
        let cfg = AppConfig::default();
        assert!(cfg.provider_api_key("deepseek").is_none());
    }
}
