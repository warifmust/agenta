use async_trait::async_trait;
use chrono::Utc;
use sqlx::{
    postgres::PgPoolOptions,
    sqlite::SqlitePoolOptions,
    Pool, Row, Sqlite, Postgres,
};
use std::path::Path;

use super::agent::{Agent, ExecutionResult, ToolExecution, ToolResource, ScriptDefinition, ScriptExecution, ScriptExecutionStatus};
use super::proposal::{Proposal, ProposalStatus};
use super::error::{AgentaError, Result};

/// Storage trait - can be implemented for different backends
#[async_trait]
pub trait Storage: Send + Sync {
    /// Create a new agent
    async fn create_agent(&self, agent: &Agent) -> Result<()>;

    /// Get an agent by ID
    async fn get_agent(&self, id: &str) -> Result<Option<Agent>>;

    /// Get an agent by name
    async fn get_agent_by_name(&self, name: &str) -> Result<Option<Agent>>;

    /// Update an agent
    async fn update_agent(&self, agent: &Agent) -> Result<()>;

    /// Delete an agent
    async fn delete_agent(&self, id: &str) -> Result<bool>;

    /// List all agents
    async fn list_agents(&self) -> Result<Vec<Agent>>;

    /// List agents by status
    async fn list_agents_by_status(&self, status: &str) -> Result<Vec<Agent>>;

    /// Create execution result
    async fn create_execution(&self, execution: &ExecutionResult) -> Result<()>;

    /// Update execution result
    async fn update_execution(&self, execution: &ExecutionResult) -> Result<()>;

    /// Get execution by ID
    async fn get_execution(&self, id: &str) -> Result<Option<ExecutionResult>>;

    /// List executions for an agent
    async fn list_executions(&self, agent_id: &str, limit: i64) -> Result<Vec<ExecutionResult>>;

    /// Cancel all running executions for an agent
    async fn cancel_running_executions(&self, agent_id: &str) -> Result<()>;

    /// Get active scheduled agents
    async fn get_scheduled_agents(&self) -> Result<Vec<Agent>>;

    /// Get triggered agents
    async fn get_triggered_agents(&self) -> Result<Vec<Agent>>;

    /// Create a tool
    async fn create_tool(&self, tool: &ToolResource) -> Result<()>;

    /// Get tool by ID
    async fn get_tool(&self, id: &str) -> Result<Option<ToolResource>>;

    /// Get tool by name
    async fn get_tool_by_name(&self, name: &str) -> Result<Option<ToolResource>>;

    /// Update tool
    async fn update_tool(&self, tool: &ToolResource) -> Result<()>;

    /// Delete tool
    async fn delete_tool(&self, id: &str) -> Result<bool>;

    /// List tools
    async fn list_tools(&self) -> Result<Vec<ToolResource>>;

    /// Create tool execution
    async fn create_tool_execution(&self, execution: &ToolExecution) -> Result<()>;

    /// Update tool execution
    async fn update_tool_execution(&self, execution: &ToolExecution) -> Result<()>;

    /// Get tool execution
    async fn get_tool_execution(&self, id: &str) -> Result<Option<ToolExecution>>;

    /// List executions for a tool
    async fn list_tool_executions(&self, tool_id: &str, limit: i64) -> Result<Vec<ToolExecution>>;

    /// Create a proposal (a human-gated mutation drafted by an agent)
    async fn create_proposal(&self, proposal: &Proposal) -> Result<()>;

    /// Get a proposal by id (accepts a unique id prefix)
    async fn get_proposal(&self, id: &str) -> Result<Option<Proposal>>;

    /// Update a proposal (status/result after a decision)
    async fn update_proposal(&self, proposal: &Proposal) -> Result<()>;

    /// List proposals, optionally filtered to a single status, newest first
    async fn list_proposals(&self, status: Option<ProposalStatus>) -> Result<Vec<Proposal>>;

    /// Create a script
    async fn create_script(&self, script: &ScriptDefinition) -> Result<()>;

    /// Get script by ID or name
    async fn get_script(&self, id: &str) -> Result<Option<ScriptDefinition>>;

    /// Get script by name
    async fn get_script_by_name(&self, name: &str) -> Result<Option<ScriptDefinition>>;

    /// Update script
    async fn update_script(&self, script: &ScriptDefinition) -> Result<()>;

    /// Delete script
    async fn delete_script(&self, id: &str) -> Result<bool>;

    /// List all scripts
    async fn list_scripts(&self) -> Result<Vec<ScriptDefinition>>;

    /// Get scheduled scripts
    async fn get_scheduled_scripts(&self) -> Result<Vec<ScriptDefinition>>;

    /// Create script execution
    async fn create_script_execution(&self, execution: &ScriptExecution) -> Result<()>;

    /// Update script execution
    async fn update_script_execution(&self, execution: &ScriptExecution) -> Result<()>;

    /// Get script execution by ID
    async fn get_script_execution(&self, id: &str) -> Result<Option<ScriptExecution>>;

    /// List executions for a script
    async fn list_script_executions(&self, script_id: &str, limit: i64) -> Result<Vec<ScriptExecution>>;
}

/// SQLite storage implementation
pub struct SqliteStorage {
    pool: Pool<Sqlite>,
}

impl SqliteStorage {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;

        let storage = Self { pool };
        storage.init().await?;
        Ok(storage)
    }

    pub async fn from_path(path: &Path) -> Result<Self> {
        let database_url = format!("sqlite:{}", path.display());
        Self::new(&database_url).await
    }

    async fn init(&self) -> Result<()> {
        // Create agents table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                description TEXT,
                model TEXT NOT NULL,
                system_prompt TEXT NOT NULL,
                config TEXT NOT NULL,
                tools TEXT NOT NULL,
                execution_mode TEXT NOT NULL,
                trigger TEXT,
                schedule TEXT,
                scheduled_input TEXT,
                deep_agent_config TEXT,
                environment TEXT NOT NULL,
                memory_enabled INTEGER NOT NULL DEFAULT 0,
                is_system INTEGER NOT NULL DEFAULT 0,
                provider TEXT,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                last_run TEXT,
                run_count INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Migration: add memory_enabled column if it doesn't exist (SQLite)
        let _ = sqlx::query(
            "ALTER TABLE agents ADD COLUMN memory_enabled INTEGER NOT NULL DEFAULT 0"
        )
        .execute(&self.pool)
        .await;

        // Migration: add provider column if it doesn't exist (SQLite)
        let _ = sqlx::query(
            "ALTER TABLE agents ADD COLUMN provider TEXT"
        )
        .execute(&self.pool)
        .await;

        // Migration: add is_system column if it doesn't exist (SQLite)
        let _ = sqlx::query(
            "ALTER TABLE agents ADD COLUMN is_system INTEGER NOT NULL DEFAULT 0"
        )
        .execute(&self.pool)
        .await;

        // Migration: add scheduled_input column if it doesn't exist (SQLite)
        let _ = sqlx::query(
            "ALTER TABLE agents ADD COLUMN scheduled_input TEXT"
        )
        .execute(&self.pool)
        .await;

        // Create executions table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS executions (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                input TEXT NOT NULL,
                output TEXT,
                status TEXT NOT NULL,
                iterations INTEGER NOT NULL DEFAULT 0,
                tool_calls TEXT NOT NULL,
                error TEXT,
                metadata TEXT NOT NULL,
                FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create indexes
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_agents_name ON agents(name)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_agents_status ON agents(status)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_executions_agent_id ON executions(agent_id)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tools (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                description TEXT NOT NULL,
                parameters TEXT NOT NULL,
                handler TEXT,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Migration: secrets allowlist + side-effect classification + http handler (SQLite).
        let _ = sqlx::query("ALTER TABLE tools ADD COLUMN secrets TEXT")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE tools ADD COLUMN side_effect TEXT")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE tools ADD COLUMN http_config TEXT")
            .execute(&self.pool)
            .await;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tool_executions (
                id TEXT PRIMARY KEY,
                tool_id TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                input TEXT NOT NULL,
                output TEXT,
                status TEXT NOT NULL,
                error TEXT,
                FOREIGN KEY (tool_id) REFERENCES tools(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tools_name ON tools(name)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tool_exec_tool_id ON tool_executions(tool_id)")
            .execute(&self.pool)
            .await?;

        // Proposals: human-gated mutations drafted by agents (MIND). SQLite.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS proposals (
                id TEXT PRIMARY KEY,
                action TEXT NOT NULL,
                rationale TEXT NOT NULL,
                risk TEXT NOT NULL,
                status TEXT NOT NULL,
                proposed_by TEXT NOT NULL,
                created_at TEXT NOT NULL,
                resolved_at TEXT,
                result TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_proposals_status ON proposals(status)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS scripts (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                description TEXT,
                handler TEXT NOT NULL,
                schedule TEXT,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                last_run TEXT,
                run_count INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS script_executions (
                id TEXT PRIMARY KEY,
                script_id TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                exit_code INTEGER,
                output TEXT,
                stderr TEXT,
                status TEXT NOT NULL,
                error TEXT,
                triggered_by TEXT NOT NULL DEFAULT 'manual',
                FOREIGN KEY (script_id) REFERENCES scripts(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_scripts_name ON scripts(name)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_script_exec_script_id ON script_executions(script_id)")
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}

/// Postgres storage implementation
pub struct PostgresStorage {
    pool: Pool<Postgres>,
}

impl PostgresStorage {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;

        let storage = Self { pool };
        storage.init().await?;
        Ok(storage)
    }

    async fn init(&self) -> Result<()> {
        // Create agents table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                description TEXT,
                model TEXT NOT NULL,
                system_prompt TEXT NOT NULL,
                config TEXT NOT NULL,
                tools TEXT NOT NULL,
                execution_mode TEXT NOT NULL,
                trigger TEXT,
                schedule TEXT,
                scheduled_input TEXT,
                deep_agent_config TEXT,
                environment TEXT NOT NULL,
                memory_enabled BOOLEAN NOT NULL DEFAULT FALSE,
                is_system BOOLEAN NOT NULL DEFAULT FALSE,
                provider TEXT,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                last_run TEXT,
                run_count BIGINT NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Migration: add memory_enabled column if it doesn't exist (Postgres)
        let _ = sqlx::query(
            "ALTER TABLE agents ADD COLUMN IF NOT EXISTS memory_enabled BOOLEAN NOT NULL DEFAULT FALSE"
        )
        .execute(&self.pool)
        .await;

        // Migration: add provider column if it doesn't exist (Postgres)
        let _ = sqlx::query(
            "ALTER TABLE agents ADD COLUMN IF NOT EXISTS provider TEXT"
        )
        .execute(&self.pool)
        .await;

        // Migration: add is_system column if it doesn't exist (Postgres)
        let _ = sqlx::query(
            "ALTER TABLE agents ADD COLUMN IF NOT EXISTS is_system BOOLEAN NOT NULL DEFAULT FALSE"
        )
        .execute(&self.pool)
        .await;

        // Migration: add scheduled_input column if it doesn't exist (Postgres)
        let _ = sqlx::query(
            "ALTER TABLE agents ADD COLUMN IF NOT EXISTS scheduled_input TEXT"
        )
        .execute(&self.pool)
        .await;

        // Create executions table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS executions (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                input TEXT NOT NULL,
                output TEXT,
                status TEXT NOT NULL,
                iterations INTEGER NOT NULL DEFAULT 0,
                tool_calls TEXT NOT NULL,
                error TEXT,
                metadata TEXT NOT NULL,
                FOREIGN KEY (agent_id) REFERENCES agents(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Create indexes
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_agents_name ON agents(name)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_agents_status ON agents(status)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_executions_agent_id ON executions(agent_id)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tools (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                description TEXT NOT NULL,
                parameters TEXT NOT NULL,
                handler TEXT,
                enabled BOOLEAN NOT NULL DEFAULT TRUE,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Migration: secrets allowlist + side-effect classification + http handler (Postgres).
        let _ = sqlx::query("ALTER TABLE tools ADD COLUMN IF NOT EXISTS secrets TEXT")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE tools ADD COLUMN IF NOT EXISTS side_effect TEXT")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE tools ADD COLUMN IF NOT EXISTS http_config TEXT")
            .execute(&self.pool)
            .await;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tool_executions (
                id TEXT PRIMARY KEY,
                tool_id TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                input TEXT NOT NULL,
                output TEXT,
                status TEXT NOT NULL,
                error TEXT,
                FOREIGN KEY (tool_id) REFERENCES tools(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tools_name ON tools(name)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_tool_exec_tool_id ON tool_executions(tool_id)")
            .execute(&self.pool)
            .await?;

        // Proposals: human-gated mutations drafted by agents (MIND). Postgres.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS proposals (
                id TEXT PRIMARY KEY,
                action TEXT NOT NULL,
                rationale TEXT NOT NULL,
                risk TEXT NOT NULL,
                status TEXT NOT NULL,
                proposed_by TEXT NOT NULL,
                created_at TEXT NOT NULL,
                resolved_at TEXT,
                result TEXT
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_proposals_status ON proposals(status)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS scripts (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                description TEXT,
                handler TEXT NOT NULL,
                schedule TEXT,
                enabled BOOLEAN NOT NULL DEFAULT TRUE,
                created_at TIMESTAMPTZ NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL,
                last_run TIMESTAMPTZ,
                run_count BIGINT NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS script_executions (
                id TEXT PRIMARY KEY,
                script_id TEXT NOT NULL,
                started_at TIMESTAMPTZ NOT NULL,
                completed_at TIMESTAMPTZ,
                exit_code INTEGER,
                output TEXT,
                stderr TEXT,
                status TEXT NOT NULL,
                error TEXT,
                triggered_by TEXT NOT NULL DEFAULT 'manual',
                FOREIGN KEY (script_id) REFERENCES scripts(id) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_scripts_name ON scripts(name)")
            .execute(&self.pool)
            .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_script_exec_script_id ON script_executions(script_id)")
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn create_agent(
        &self, agent: &Agent
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO agents (
                id, name, description, model, system_prompt, config, tools,
                execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
            "#,
        )
        .bind(&agent.id)
        .bind(&agent.name)
        .bind(&agent.description)
        .bind(&agent.model)
        .bind(&agent.system_prompt)
        .bind(serde_json::to_string(&agent.config)?)
        .bind(serde_json::to_string(&agent.tools)?)
        .bind(serde_json::to_string(&agent.execution_mode)?)
        .bind(serde_json::to_string(&agent.trigger)?)
        .bind(&agent.schedule)
        .bind(&agent.scheduled_input)
        .bind(serde_json::to_string(&agent.deep_agent_config)?)
        .bind(serde_json::to_string(&agent.environment)?)
        .bind(if agent.memory_enabled { 1i64 } else { 0i64 })
        .bind(if agent.is_system { 1i64 } else { 0i64 })
        .bind(&agent.provider)
        .bind(serde_json::to_string(&agent.status)?)
        .bind(agent.created_at.to_rfc3339())
        .bind(agent.updated_at.to_rfc3339())
        .bind(agent.last_run.map(|d| d.to_rfc3339()))
        .bind(agent.run_count as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get_agent(
        &self, id: &str
    ) -> Result<Option<Agent>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| row_to_agent_sqlite(&r)))
    }

    async fn get_agent_by_name(
        &self, name: &str
    ) -> Result<Option<Agent>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents WHERE name = ?1
            "#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| row_to_agent_sqlite(&r)))
    }

    async fn update_agent(
        &self, agent: &Agent
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE agents SET
                name = ?1,
                description = ?2,
                model = ?3,
                system_prompt = ?4,
                config = ?5,
                tools = ?6,
                execution_mode = ?7,
                trigger = ?8,
                schedule = ?9,
                deep_agent_config = ?10,
                environment = ?11,
                memory_enabled = ?12,
                provider = ?13,
                status = ?14,
                updated_at = ?15,
                last_run = ?16,
                run_count = ?17,
                scheduled_input = ?18
            WHERE id = ?19
            "#,
        )
        .bind(&agent.name)
        .bind(&agent.description)
        .bind(&agent.model)
        .bind(&agent.system_prompt)
        .bind(serde_json::to_string(&agent.config)?)
        .bind(serde_json::to_string(&agent.tools)?)
        .bind(serde_json::to_string(&agent.execution_mode)?)
        .bind(serde_json::to_string(&agent.trigger)?)
        .bind(&agent.schedule)
        .bind(serde_json::to_string(&agent.deep_agent_config)?)
        .bind(serde_json::to_string(&agent.environment)?)
        .bind(if agent.memory_enabled { 1i64 } else { 0i64 })
        .bind(&agent.provider)
        .bind(serde_json::to_string(&agent.status)?)
        .bind(agent.updated_at.to_rfc3339())
        .bind(agent.last_run.map(|d| d.to_rfc3339()))
        .bind(agent.run_count as i64)
        .bind(&agent.scheduled_input)
        .bind(&agent.id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete_agent(
        &self, id: &str
    ) -> Result<bool> {
        // Refuse to delete system agents
        let is_sys: Option<i64> = sqlx::query_scalar(
            "SELECT is_system FROM agents WHERE id = ?1 OR name = ?1"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        if is_sys == Some(1) {
            return Err(AgentaError::SystemAgent("system agents cannot be deleted".into()));
        }
        let result = sqlx::query("DELETE FROM agents WHERE id = ?1 OR name = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_agents(
        &self
    ) -> Result<Vec<Agent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents WHERE is_system = 0 ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(|r| row_to_agent_sqlite(&r)).collect())
    }

    async fn list_agents_by_status(
        &self, status: &str
    ) -> Result<Vec<Agent>> {
        let status_value = serde_json::to_string(&status)?;
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents WHERE status = ?1 ORDER BY created_at DESC
            "#,
        )
        .bind(status_value)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(|r| row_to_agent_sqlite(&r)).collect())
    }

    async fn create_execution(
        &self, execution: &ExecutionResult
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO executions (
                id, agent_id, started_at, completed_at, input, output,
                status, iterations, tool_calls, error, metadata
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
        )
        .bind(&execution.id)
        .bind(&execution.agent_id)
        .bind(execution.started_at.to_rfc3339())
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(&execution.input)
        .bind(&execution.output)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(execution.iterations as i64)
        .bind(serde_json::to_string(&execution.tool_calls)?)
        .bind(&execution.error)
        .bind(serde_json::to_string(&execution.metadata)?)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn update_execution(
        &self, execution: &ExecutionResult
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE executions SET
                completed_at = ?1,
                output = ?2,
                status = ?3,
                iterations = ?4,
                tool_calls = ?5,
                error = ?6,
                metadata = ?7
            WHERE id = ?8
            "#,
        )
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(&execution.output)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(execution.iterations as i64)
        .bind(serde_json::to_string(&execution.tool_calls)?)
        .bind(&execution.error)
        .bind(serde_json::to_string(&execution.metadata)?)
        .bind(&execution.id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get_execution(
        &self, id: &str
    ) -> Result<Option<ExecutionResult>> {
        let row = sqlx::query(
            r#"
            SELECT id, agent_id, started_at, completed_at, input, output,
                   status, iterations, tool_calls, error, metadata
            FROM executions WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| row_to_execution_sqlite(&r)))
    }

    async fn list_executions(
        &self, agent_id: &str, limit: i64
    ) -> Result<Vec<ExecutionResult>> {
        let rows = sqlx::query(
            r#"
            SELECT id, agent_id, started_at, completed_at, input, output,
                   status, iterations, tool_calls, error, metadata
            FROM executions
            WHERE agent_id = ?1
            ORDER BY started_at DESC
            LIMIT ?2
            "#,
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(|r| row_to_execution_sqlite(&r)).collect())
    }

    async fn cancel_running_executions(&self, agent_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            UPDATE executions
            SET status = '"cancelled"', completed_at = ?1
            WHERE agent_id = ?2 AND status = '"running"'
            "#,
        )
        .bind(&now)
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn create_tool(&self, tool: &ToolResource) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO tools (id, name, description, parameters, handler, enabled, secrets, side_effect, http_config, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
        )
        .bind(&tool.id)
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(serde_json::to_string(&tool.parameters)?)
        .bind(&tool.handler)
        .bind(if tool.enabled { 1i64 } else { 0i64 })
        .bind(serde_json::to_string(&tool.secrets)?)
        .bind(serde_json::to_string(&tool.side_effect)?)
        .bind(tool.http.as_ref().map(serde_json::to_string).transpose()?)
        .bind(tool.created_at.to_rfc3339())
        .bind(tool.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_tool(&self, id: &str) -> Result<Option<ToolResource>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, parameters, handler, enabled, secrets, side_effect, http_config, created_at, updated_at
            FROM tools WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_tool_sqlite(&r)))
    }

    async fn get_tool_by_name(&self, name: &str) -> Result<Option<ToolResource>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, parameters, handler, enabled, secrets, side_effect, http_config, created_at, updated_at
            FROM tools WHERE name = ?1
            "#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_tool_sqlite(&r)))
    }

    async fn update_tool(&self, tool: &ToolResource) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE tools SET
                name = ?1,
                description = ?2,
                parameters = ?3,
                handler = ?4,
                enabled = ?5,
                secrets = ?6,
                side_effect = ?7,
                http_config = ?8,
                updated_at = ?9
            WHERE id = ?10
            "#,
        )
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(serde_json::to_string(&tool.parameters)?)
        .bind(&tool.handler)
        .bind(if tool.enabled { 1i64 } else { 0i64 })
        .bind(serde_json::to_string(&tool.secrets)?)
        .bind(serde_json::to_string(&tool.side_effect)?)
        .bind(tool.http.as_ref().map(serde_json::to_string).transpose()?)
        .bind(tool.updated_at.to_rfc3339())
        .bind(&tool.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_tool(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM tools WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_tools(&self) -> Result<Vec<ToolResource>> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, parameters, handler, enabled, secrets, side_effect, http_config, created_at, updated_at
            FROM tools
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| row_to_tool_sqlite(&r)).collect())
    }

    async fn create_tool_execution(&self, execution: &ToolExecution) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO tool_executions (id, tool_id, started_at, completed_at, input, output, status, error)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(&execution.id)
        .bind(&execution.tool_id)
        .bind(execution.started_at.to_rfc3339())
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(serde_json::to_string(&execution.input)?)
        .bind(&execution.output)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(&execution.error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_tool_execution(&self, execution: &ToolExecution) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE tool_executions SET
                completed_at = ?1,
                output = ?2,
                status = ?3,
                error = ?4
            WHERE id = ?5
            "#,
        )
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(&execution.output)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(&execution.error)
        .bind(&execution.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_tool_execution(&self, id: &str) -> Result<Option<ToolExecution>> {
        let row = sqlx::query(
            r#"
            SELECT id, tool_id, started_at, completed_at, input, output, status, error
            FROM tool_executions
            WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_tool_execution_sqlite(&r)))
    }

    async fn list_tool_executions(&self, tool_id: &str, limit: i64) -> Result<Vec<ToolExecution>> {
        let rows = sqlx::query(
            r#"
            SELECT id, tool_id, started_at, completed_at, input, output, status, error
            FROM tool_executions
            WHERE tool_id = ?1
            ORDER BY started_at DESC
            LIMIT ?2
            "#,
        )
        .bind(tool_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| row_to_tool_execution_sqlite(&r))
            .collect())
    }

    async fn create_proposal(&self, proposal: &Proposal) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO proposals (id, action, rationale, risk, status, proposed_by, created_at, resolved_at, result)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
        )
        .bind(&proposal.id)
        .bind(serde_json::to_string(&proposal.action)?)
        .bind(&proposal.rationale)
        .bind(serde_json::to_string(&proposal.risk)?)
        .bind(serde_json::to_string(&proposal.status)?)
        .bind(&proposal.proposed_by)
        .bind(proposal.created_at.to_rfc3339())
        .bind(proposal.resolved_at.map(|d| d.to_rfc3339()))
        .bind(&proposal.result)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_proposal(&self, id: &str) -> Result<Option<Proposal>> {
        let row = sqlx::query(
            r#"
            SELECT id, action, rationale, risk, status, proposed_by, created_at, resolved_at, result
            FROM proposals WHERE id = ?1 OR id LIKE ?2
            LIMIT 1
            "#,
        )
        .bind(id)
        .bind(format!("{}%", id))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_proposal_sqlite(&r)))
    }

    async fn update_proposal(&self, proposal: &Proposal) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE proposals SET status = ?1, resolved_at = ?2, result = ?3 WHERE id = ?4
            "#,
        )
        .bind(serde_json::to_string(&proposal.status)?)
        .bind(proposal.resolved_at.map(|d| d.to_rfc3339()))
        .bind(&proposal.result)
        .bind(&proposal.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_proposals(&self, status: Option<ProposalStatus>) -> Result<Vec<Proposal>> {
        let rows = match status {
            Some(s) => {
                sqlx::query(
                    r#"
                    SELECT id, action, rationale, risk, status, proposed_by, created_at, resolved_at, result
                    FROM proposals WHERE status = ?1 ORDER BY created_at DESC
                    "#,
                )
                .bind(serde_json::to_string(&s)?)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    r#"
                    SELECT id, action, rationale, risk, status, proposed_by, created_at, resolved_at, result
                    FROM proposals ORDER BY created_at DESC
                    "#,
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().filter_map(|r| row_to_proposal_sqlite(&r)).collect())
    }

    async fn create_script(&self, script: &ScriptDefinition) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO scripts (id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
        )
        .bind(&script.id)
        .bind(&script.name)
        .bind(&script.description)
        .bind(&script.handler)
        .bind(&script.schedule)
        .bind(script.enabled as i64)
        .bind(script.created_at.to_rfc3339())
        .bind(script.updated_at.to_rfc3339())
        .bind(script.last_run.map(|d| d.to_rfc3339()))
        .bind(script.run_count as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_script(&self, id: &str) -> Result<Option<ScriptDefinition>> {
        let row = sqlx::query(
            "SELECT id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count FROM scripts WHERE id = ?1 OR name = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_script_sqlite(&r)))
    }

    async fn get_script_by_name(&self, name: &str) -> Result<Option<ScriptDefinition>> {
        let row = sqlx::query(
            "SELECT id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count FROM scripts WHERE name = ?1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_script_sqlite(&r)))
    }

    async fn update_script(&self, script: &ScriptDefinition) -> Result<()> {
        sqlx::query(
            r#"UPDATE scripts SET name=?1, description=?2, handler=?3, schedule=?4, enabled=?5, updated_at=?6, last_run=?7, run_count=?8 WHERE id=?9"#,
        )
        .bind(&script.name)
        .bind(&script.description)
        .bind(&script.handler)
        .bind(&script.schedule)
        .bind(script.enabled as i64)
        .bind(script.updated_at.to_rfc3339())
        .bind(script.last_run.map(|d| d.to_rfc3339()))
        .bind(script.run_count as i64)
        .bind(&script.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_script(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM scripts WHERE id = ?1 OR name = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_scripts(&self) -> Result<Vec<ScriptDefinition>> {
        let rows = sqlx::query(
            "SELECT id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count FROM scripts ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| row_to_script_sqlite(&r)).collect())
    }

    async fn get_scheduled_scripts(&self) -> Result<Vec<ScriptDefinition>> {
        let rows = sqlx::query(
            "SELECT id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count FROM scripts WHERE enabled = 1 AND schedule IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| row_to_script_sqlite(&r)).collect())
    }

    async fn create_script_execution(&self, execution: &ScriptExecution) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO script_executions (id, script_id, started_at, completed_at, exit_code, output, stderr, status, error, triggered_by)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
        )
        .bind(&execution.id)
        .bind(&execution.script_id)
        .bind(execution.started_at.to_rfc3339())
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(execution.exit_code)
        .bind(&execution.output)
        .bind(&execution.stderr)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(&execution.error)
        .bind(&execution.triggered_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_script_execution(&self, execution: &ScriptExecution) -> Result<()> {
        sqlx::query(
            r#"UPDATE script_executions SET completed_at=?1, exit_code=?2, output=?3, stderr=?4, status=?5, error=?6 WHERE id=?7"#,
        )
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(execution.exit_code)
        .bind(&execution.output)
        .bind(&execution.stderr)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(&execution.error)
        .bind(&execution.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_script_execution(&self, id: &str) -> Result<Option<ScriptExecution>> {
        let row = sqlx::query(
            "SELECT id, script_id, started_at, completed_at, exit_code, output, stderr, status, error, triggered_by FROM script_executions WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_script_execution_sqlite(&r)))
    }

    async fn list_script_executions(&self, script_id: &str, limit: i64) -> Result<Vec<ScriptExecution>> {
        let rows = sqlx::query(
            "SELECT id, script_id, started_at, completed_at, exit_code, output, stderr, status, error, triggered_by FROM script_executions WHERE script_id = ?1 ORDER BY started_at DESC LIMIT ?2",
        )
        .bind(script_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| row_to_script_execution_sqlite(&r)).collect())
    }

    async fn get_scheduled_agents(
        &self
    ) -> Result<Vec<Agent>> {
        let agents = self.list_agents_by_status("active").await?;
        Ok(agents
            .into_iter()
            .filter(|agent| matches!(agent.execution_mode, crate::core::ExecutionMode::Scheduled))
            .filter(|agent| agent.schedule.is_some())
            .collect())
    }

    async fn get_triggered_agents(
        &self
    ) -> Result<Vec<Agent>> {
        let status_value = serde_json::to_string("active")?;
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents
            WHERE trigger IS NOT NULL AND status = ?1
            "#,
        )
        .bind(status_value)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(|r| row_to_agent_sqlite(&r)).collect())
    }
}

#[async_trait]
impl Storage for PostgresStorage {
    async fn create_agent(
        &self, agent: &Agent
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO agents (
                id, name, description, model, system_prompt, config, tools,
                execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)
            "#,
        )
        .bind(&agent.id)
        .bind(&agent.name)
        .bind(&agent.description)
        .bind(&agent.model)
        .bind(&agent.system_prompt)
        .bind(serde_json::to_string(&agent.config)?)
        .bind(serde_json::to_string(&agent.tools)?)
        .bind(serde_json::to_string(&agent.execution_mode)?)
        .bind(serde_json::to_string(&agent.trigger)?)
        .bind(&agent.schedule)
        .bind(&agent.scheduled_input)
        .bind(serde_json::to_string(&agent.deep_agent_config)?)
        .bind(serde_json::to_string(&agent.environment)?)
        .bind(agent.memory_enabled)
        .bind(agent.is_system)
        .bind(&agent.provider)
        .bind(serde_json::to_string(&agent.status)?)
        .bind(agent.created_at.to_rfc3339())
        .bind(agent.updated_at.to_rfc3339())
        .bind(agent.last_run.map(|d| d.to_rfc3339()))
        .bind(agent.run_count as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get_agent(
        &self, id: &str
    ) -> Result<Option<Agent>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| row_to_agent_pg(&r)))
    }

    async fn get_agent_by_name(
        &self, name: &str
    ) -> Result<Option<Agent>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents WHERE name = $1
            "#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| row_to_agent_pg(&r)))
    }

    async fn update_agent(
        &self, agent: &Agent
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE agents SET
                name = $1,
                description = $2,
                model = $3,
                system_prompt = $4,
                config = $5,
                tools = $6,
                execution_mode = $7,
                trigger = $8,
                schedule = $9,
                deep_agent_config = $10,
                environment = $11,
                memory_enabled = $12,
                provider = $13,
                status = $14,
                updated_at = $15,
                last_run = $16,
                run_count = $17,
                scheduled_input = $18
            WHERE id = $19
            "#,
        )
        .bind(&agent.name)
        .bind(&agent.description)
        .bind(&agent.model)
        .bind(&agent.system_prompt)
        .bind(serde_json::to_string(&agent.config)?)
        .bind(serde_json::to_string(&agent.tools)?)
        .bind(serde_json::to_string(&agent.execution_mode)?)
        .bind(serde_json::to_string(&agent.trigger)?)
        .bind(&agent.schedule)
        .bind(serde_json::to_string(&agent.deep_agent_config)?)
        .bind(serde_json::to_string(&agent.environment)?)
        .bind(agent.memory_enabled)
        .bind(&agent.provider)
        .bind(serde_json::to_string(&agent.status)?)
        .bind(agent.updated_at.to_rfc3339())
        .bind(agent.last_run.map(|d| d.to_rfc3339()))
        .bind(agent.run_count as i64)
        .bind(&agent.scheduled_input)
        .bind(&agent.id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete_agent(
        &self, id: &str
    ) -> Result<bool> {
        // Refuse to delete system agents
        let is_sys: Option<bool> = sqlx::query_scalar(
            "SELECT is_system FROM agents WHERE id = $1 OR name = $1"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        if is_sys == Some(true) {
            return Err(AgentaError::SystemAgent("system agents cannot be deleted".into()));
        }
        let result = sqlx::query("DELETE FROM agents WHERE id = $1 OR name = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_agents(
        &self
    ) -> Result<Vec<Agent>> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents WHERE is_system = FALSE ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(|r| row_to_agent_pg(&r)).collect())
    }

    async fn list_agents_by_status(
        &self, status: &str
    ) -> Result<Vec<Agent>> {
        let status_value = serde_json::to_string(&status)?;
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents WHERE status = $1 ORDER BY created_at DESC
            "#,
        )
        .bind(status_value)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(|r| row_to_agent_pg(&r)).collect())
    }

    async fn create_execution(
        &self, execution: &ExecutionResult
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO executions (
                id, agent_id, started_at, completed_at, input, output,
                status, iterations, tool_calls, error, metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
        )
        .bind(&execution.id)
        .bind(&execution.agent_id)
        .bind(execution.started_at.to_rfc3339())
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(&execution.input)
        .bind(&execution.output)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(execution.iterations as i64)
        .bind(serde_json::to_string(&execution.tool_calls)?)
        .bind(&execution.error)
        .bind(serde_json::to_string(&execution.metadata)?)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn update_execution(
        &self, execution: &ExecutionResult
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE executions SET
                completed_at = $1,
                output = $2,
                status = $3,
                iterations = $4,
                tool_calls = $5,
                error = $6,
                metadata = $7
            WHERE id = $8
            "#,
        )
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(&execution.output)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(execution.iterations as i64)
        .bind(serde_json::to_string(&execution.tool_calls)?)
        .bind(&execution.error)
        .bind(serde_json::to_string(&execution.metadata)?)
        .bind(&execution.id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn get_execution(
        &self, id: &str
    ) -> Result<Option<ExecutionResult>> {
        let row = sqlx::query(
            r#"
            SELECT id, agent_id, started_at, completed_at, input, output,
                   status, iterations, tool_calls, error, metadata
            FROM executions WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| row_to_execution_pg(&r)))
    }

    async fn list_executions(
        &self, agent_id: &str, limit: i64
    ) -> Result<Vec<ExecutionResult>> {
        let rows = sqlx::query(
            r#"
            SELECT id, agent_id, started_at, completed_at, input, output,
                   status, iterations, tool_calls, error, metadata
            FROM executions
            WHERE agent_id = $1
            ORDER BY started_at DESC
            LIMIT $2
            "#,
        )
        .bind(agent_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(|r| row_to_execution_pg(&r)).collect())
    }

    async fn cancel_running_executions(&self, agent_id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            r#"
            UPDATE executions
            SET status = '"cancelled"', completed_at = $1
            WHERE agent_id = $2 AND status = '"running"'
            "#,
        )
        .bind(&now)
        .bind(agent_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn create_tool(&self, tool: &ToolResource) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO tools (id, name, description, parameters, handler, enabled, secrets, side_effect, http_config, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
        )
        .bind(&tool.id)
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(serde_json::to_string(&tool.parameters)?)
        .bind(&tool.handler)
        .bind(tool.enabled)
        .bind(serde_json::to_string(&tool.secrets)?)
        .bind(serde_json::to_string(&tool.side_effect)?)
        .bind(tool.http.as_ref().map(serde_json::to_string).transpose()?)
        .bind(tool.created_at.to_rfc3339())
        .bind(tool.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_tool(&self, id: &str) -> Result<Option<ToolResource>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, parameters, handler, enabled, secrets, side_effect, http_config, created_at, updated_at
            FROM tools WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_tool_pg(&r)))
    }

    async fn get_tool_by_name(&self, name: &str) -> Result<Option<ToolResource>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, parameters, handler, enabled, secrets, side_effect, http_config, created_at, updated_at
            FROM tools WHERE name = $1
            "#,
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_tool_pg(&r)))
    }

    async fn update_tool(&self, tool: &ToolResource) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE tools SET
                name = $1,
                description = $2,
                parameters = $3,
                handler = $4,
                enabled = $5,
                secrets = $6,
                side_effect = $7,
                http_config = $8,
                updated_at = $9
            WHERE id = $10
            "#,
        )
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(serde_json::to_string(&tool.parameters)?)
        .bind(&tool.handler)
        .bind(tool.enabled)
        .bind(serde_json::to_string(&tool.secrets)?)
        .bind(serde_json::to_string(&tool.side_effect)?)
        .bind(tool.http.as_ref().map(serde_json::to_string).transpose()?)
        .bind(tool.updated_at.to_rfc3339())
        .bind(&tool.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_tool(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM tools WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_tools(&self) -> Result<Vec<ToolResource>> {
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, parameters, handler, enabled, secrets, side_effect, http_config, created_at, updated_at
            FROM tools
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| row_to_tool_pg(&r)).collect())
    }

    async fn create_tool_execution(&self, execution: &ToolExecution) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO tool_executions (id, tool_id, started_at, completed_at, input, output, status, error)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(&execution.id)
        .bind(&execution.tool_id)
        .bind(execution.started_at.to_rfc3339())
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(serde_json::to_string(&execution.input)?)
        .bind(&execution.output)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(&execution.error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_tool_execution(&self, execution: &ToolExecution) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE tool_executions SET
                completed_at = $1,
                output = $2,
                status = $3,
                error = $4
            WHERE id = $5
            "#,
        )
        .bind(execution.completed_at.map(|d| d.to_rfc3339()))
        .bind(&execution.output)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(&execution.error)
        .bind(&execution.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_tool_execution(&self, id: &str) -> Result<Option<ToolExecution>> {
        let row = sqlx::query(
            r#"
            SELECT id, tool_id, started_at, completed_at, input, output, status, error
            FROM tool_executions
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_tool_execution_pg(&r)))
    }

    async fn list_tool_executions(&self, tool_id: &str, limit: i64) -> Result<Vec<ToolExecution>> {
        let rows = sqlx::query(
            r#"
            SELECT id, tool_id, started_at, completed_at, input, output, status, error
            FROM tool_executions
            WHERE tool_id = $1
            ORDER BY started_at DESC
            LIMIT $2
            "#,
        )
        .bind(tool_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| row_to_tool_execution_pg(&r))
            .collect())
    }

    async fn create_proposal(&self, proposal: &Proposal) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO proposals (id, action, rationale, risk, status, proposed_by, created_at, resolved_at, result)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(&proposal.id)
        .bind(serde_json::to_string(&proposal.action)?)
        .bind(&proposal.rationale)
        .bind(serde_json::to_string(&proposal.risk)?)
        .bind(serde_json::to_string(&proposal.status)?)
        .bind(&proposal.proposed_by)
        .bind(proposal.created_at.to_rfc3339())
        .bind(proposal.resolved_at.map(|d| d.to_rfc3339()))
        .bind(&proposal.result)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_proposal(&self, id: &str) -> Result<Option<Proposal>> {
        let row = sqlx::query(
            r#"
            SELECT id, action, rationale, risk, status, proposed_by, created_at, resolved_at, result
            FROM proposals WHERE id = $1 OR id LIKE $2
            LIMIT 1
            "#,
        )
        .bind(id)
        .bind(format!("{}%", id))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_proposal_pg(&r)))
    }

    async fn update_proposal(&self, proposal: &Proposal) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE proposals SET status = $1, resolved_at = $2, result = $3 WHERE id = $4
            "#,
        )
        .bind(serde_json::to_string(&proposal.status)?)
        .bind(proposal.resolved_at.map(|d| d.to_rfc3339()))
        .bind(&proposal.result)
        .bind(&proposal.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_proposals(&self, status: Option<ProposalStatus>) -> Result<Vec<Proposal>> {
        let rows = match status {
            Some(s) => {
                sqlx::query(
                    r#"
                    SELECT id, action, rationale, risk, status, proposed_by, created_at, resolved_at, result
                    FROM proposals WHERE status = $1 ORDER BY created_at DESC
                    "#,
                )
                .bind(serde_json::to_string(&s)?)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    r#"
                    SELECT id, action, rationale, risk, status, proposed_by, created_at, resolved_at, result
                    FROM proposals ORDER BY created_at DESC
                    "#,
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows.into_iter().filter_map(|r| row_to_proposal_pg(&r)).collect())
    }

    async fn create_script(&self, script: &ScriptDefinition) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO scripts (id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        )
        .bind(&script.id)
        .bind(&script.name)
        .bind(&script.description)
        .bind(&script.handler)
        .bind(&script.schedule)
        .bind(script.enabled)
        .bind(script.created_at)
        .bind(script.updated_at)
        .bind(script.last_run)
        .bind(script.run_count as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_script(&self, id: &str) -> Result<Option<ScriptDefinition>> {
        let row = sqlx::query(
            "SELECT id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count FROM scripts WHERE id = $1 OR name = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_script_pg(&r)))
    }

    async fn get_script_by_name(&self, name: &str) -> Result<Option<ScriptDefinition>> {
        let row = sqlx::query(
            "SELECT id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count FROM scripts WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_script_pg(&r)))
    }

    async fn update_script(&self, script: &ScriptDefinition) -> Result<()> {
        sqlx::query(
            r#"UPDATE scripts SET name=$1, description=$2, handler=$3, schedule=$4, enabled=$5, updated_at=$6, last_run=$7, run_count=$8 WHERE id=$9"#,
        )
        .bind(&script.name)
        .bind(&script.description)
        .bind(&script.handler)
        .bind(&script.schedule)
        .bind(script.enabled)
        .bind(script.updated_at)
        .bind(script.last_run)
        .bind(script.run_count as i64)
        .bind(&script.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete_script(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM scripts WHERE id = $1 OR name = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn list_scripts(&self) -> Result<Vec<ScriptDefinition>> {
        let rows = sqlx::query(
            "SELECT id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count FROM scripts ORDER BY name ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| row_to_script_pg(&r)).collect())
    }

    async fn get_scheduled_scripts(&self) -> Result<Vec<ScriptDefinition>> {
        let rows = sqlx::query(
            "SELECT id, name, description, handler, schedule, enabled, created_at, updated_at, last_run, run_count FROM scripts WHERE enabled = TRUE AND schedule IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| row_to_script_pg(&r)).collect())
    }

    async fn create_script_execution(&self, execution: &ScriptExecution) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO script_executions (id, script_id, started_at, completed_at, exit_code, output, stderr, status, error, triggered_by)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
        )
        .bind(&execution.id)
        .bind(&execution.script_id)
        .bind(execution.started_at)
        .bind(execution.completed_at)
        .bind(execution.exit_code)
        .bind(&execution.output)
        .bind(&execution.stderr)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(&execution.error)
        .bind(&execution.triggered_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_script_execution(&self, execution: &ScriptExecution) -> Result<()> {
        sqlx::query(
            r#"UPDATE script_executions SET completed_at=$1, exit_code=$2, output=$3, stderr=$4, status=$5, error=$6 WHERE id=$7"#,
        )
        .bind(execution.completed_at)
        .bind(execution.exit_code)
        .bind(&execution.output)
        .bind(&execution.stderr)
        .bind(serde_json::to_string(&execution.status)?)
        .bind(&execution.error)
        .bind(&execution.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_script_execution(&self, id: &str) -> Result<Option<ScriptExecution>> {
        let row = sqlx::query(
            "SELECT id, script_id, started_at, completed_at, exit_code, output, stderr, status, error, triggered_by FROM script_executions WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|r| row_to_script_execution_pg(&r)))
    }

    async fn list_script_executions(&self, script_id: &str, limit: i64) -> Result<Vec<ScriptExecution>> {
        let rows = sqlx::query(
            "SELECT id, script_id, started_at, completed_at, exit_code, output, stderr, status, error, triggered_by FROM script_executions WHERE script_id = $1 ORDER BY started_at DESC LIMIT $2",
        )
        .bind(script_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(|r| row_to_script_execution_pg(&r)).collect())
    }

    async fn get_scheduled_agents(
        &self
    ) -> Result<Vec<Agent>> {
        let agents = self.list_agents_by_status("active").await?;
        Ok(agents
            .into_iter()
            .filter(|agent| matches!(agent.execution_mode, crate::core::ExecutionMode::Scheduled))
            .filter(|agent| agent.schedule.is_some())
            .collect())
    }

    async fn get_triggered_agents(
        &self
    ) -> Result<Vec<Agent>> {
        let status_value = serde_json::to_string("active")?;
        let rows = sqlx::query(
            r#"
            SELECT id, name, description, model, system_prompt, config, tools,
                   execution_mode, trigger, schedule, scheduled_input, deep_agent_config, environment,
                   memory_enabled, is_system, provider, status, created_at, updated_at, last_run, run_count
            FROM agents
            WHERE trigger IS NOT NULL AND status = $1
            "#,
        )
        .bind(status_value)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().filter_map(|r| row_to_agent_pg(&r)).collect())
    }
}

fn row_to_agent_sqlite(row: &sqlx::sqlite::SqliteRow) -> Option<Agent> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_str = |col: &str| row.try_get::<&str, _>(col).ok();
    let get_optional_str = |col: &str| row.try_get::<Option<&str>, _>(col).ok().flatten();
    let get_i64 = |col: &str| row.try_get::<i64, _>(col).ok();

    Some(Agent {
        id: get_string("id")?,
        name: get_string("name")?,
        description: get_optional_str("description").map(|s| s.to_string()),
        model: get_string("model")?,
        system_prompt: get_string("system_prompt")?,
        config: serde_json::from_str(get_str("config")?).ok()?,
        tools: serde_json::from_str(get_str("tools")?).ok()?,
        execution_mode: serde_json::from_str(get_str("execution_mode")?).ok()?,
        trigger: get_optional_str("trigger").and_then(|s| serde_json::from_str(s).ok()),
        schedule: get_optional_str("schedule").map(|s| s.to_string()),
        scheduled_input: get_optional_str("scheduled_input").map(|s| s.to_string()),
        deep_agent_config: get_optional_str("deep_agent_config").and_then(|s| serde_json::from_str(s).ok()),
        environment: serde_json::from_str(get_str("environment")?).ok()?,
        memory_enabled: get_i64("memory_enabled").unwrap_or(0) != 0,
        is_system: get_i64("is_system").unwrap_or(0) != 0,
        provider: get_optional_str("provider").map(|s| s.to_string()),
        status: serde_json::from_str(get_str("status")?).ok()?,
        created_at: get_str("created_at")?.parse().ok()?,
        updated_at: get_str("updated_at")?.parse().ok()?,
        last_run: get_optional_str("last_run").and_then(|s| s.parse().ok()),
        run_count: get_i64("run_count")? as u64,
    })
}

fn row_to_execution_sqlite(row: &sqlx::sqlite::SqliteRow) -> Option<ExecutionResult> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_str = |col: &str| row.try_get::<&str, _>(col).ok();
    let get_optional_str = |col: &str| row.try_get::<Option<&str>, _>(col).ok().flatten();
    let get_i64 = |col: &str| row.try_get::<i64, _>(col).ok();

    Some(ExecutionResult {
        id: get_string("id")?,
        agent_id: get_string("agent_id")?,
        started_at: get_str("started_at")?.parse().ok()?,
        completed_at: get_optional_str("completed_at").and_then(|s| s.parse().ok()),
        input: get_string("input")?,
        output: get_optional_str("output").map(|s| s.to_string()),
        status: serde_json::from_str(get_str("status")?).ok()?,
        iterations: get_i64("iterations")? as u32,
        tool_calls: serde_json::from_str(get_str("tool_calls")?).ok()?,
        error: get_optional_str("error").map(|s| s.to_string()),
        metadata: serde_json::from_str(get_str("metadata")?).ok()?,
    })
}

fn row_to_agent_pg(row: &sqlx::postgres::PgRow) -> Option<Agent> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();
    let get_i64 = |col: &str| row.try_get::<i64, _>(col).ok();

    let config = get_string("config")?;
    let tools = get_string("tools")?;
    let execution_mode = get_string("execution_mode")?;
    let trigger = get_optional_string("trigger");
    let schedule = get_optional_string("schedule");
    let scheduled_input = get_optional_string("scheduled_input");
    let deep_agent_config = get_optional_string("deep_agent_config");
    let environment = get_string("environment")?;
    let status = get_string("status")?;
    let created_at = get_string("created_at")?;
    let updated_at = get_string("updated_at")?;
    let last_run = get_optional_string("last_run");

    Some(Agent {
        id: get_string("id")?,
        name: get_string("name")?,
        description: get_optional_string("description"),
        model: get_string("model")?,
        system_prompt: get_string("system_prompt")?,
        config: serde_json::from_str(&config).ok()?,
        tools: serde_json::from_str(&tools).ok()?,
        execution_mode: serde_json::from_str(&execution_mode).ok()?,
        trigger: trigger.as_deref().and_then(|s| serde_json::from_str(s).ok()),
        schedule,
        scheduled_input,
        deep_agent_config: deep_agent_config
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        environment: serde_json::from_str(&environment).ok()?,
        memory_enabled: row.try_get::<bool, _>("memory_enabled").unwrap_or(false),
        is_system: row.try_get::<bool, _>("is_system").unwrap_or(false),
        provider: get_optional_string("provider"),
        status: serde_json::from_str(&status).ok()?,
        created_at: created_at.parse().ok()?,
        updated_at: updated_at.parse().ok()?,
        last_run: last_run.as_deref().and_then(|s| s.parse().ok()),
        run_count: get_i64("run_count")? as u64,
    })
}

fn row_to_execution_pg(row: &sqlx::postgres::PgRow) -> Option<ExecutionResult> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();

    let started_at = get_string("started_at")?;
    let completed_at = get_optional_string("completed_at");
    let status = get_string("status")?;
    let tool_calls = get_string("tool_calls")?;
    let metadata = get_string("metadata")?;
    let iterations = row
        .try_get::<i64, _>("iterations")
        .ok()
        .or_else(|| row.try_get::<i32, _>("iterations").ok().map(|v| v as i64))?;

    Some(ExecutionResult {
        id: get_string("id")?,
        agent_id: get_string("agent_id")?,
        started_at: started_at.parse().ok()?,
        completed_at: completed_at.as_deref().and_then(|s| s.parse().ok()),
        input: get_string("input")?,
        output: get_optional_string("output"),
        status: serde_json::from_str(&status).ok()?,
        iterations: iterations as u32,
        tool_calls: serde_json::from_str(&tool_calls).ok()?,
        error: get_optional_string("error"),
        metadata: serde_json::from_str(&metadata).ok()?,
    })
}

fn row_to_tool_sqlite(row: &sqlx::sqlite::SqliteRow) -> Option<ToolResource> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_str = |col: &str| row.try_get::<&str, _>(col).ok();
    let get_optional_str = |col: &str| row.try_get::<Option<&str>, _>(col).ok().flatten();
    let get_i64 = |col: &str| row.try_get::<i64, _>(col).ok();

    Some(ToolResource {
        id: get_string("id")?,
        name: get_string("name")?,
        description: get_string("description")?,
        parameters: serde_json::from_str(get_str("parameters")?).ok()?,
        handler: get_optional_str("handler").map(|s| s.to_string()),
        enabled: get_i64("enabled").unwrap_or(1) != 0,
        secrets: get_optional_str("secrets")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default(),
        side_effect: get_optional_str("side_effect")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default(),
        http: get_optional_str("http_config")
            .and_then(|s| serde_json::from_str(s).ok()),
        created_at: get_str("created_at")?.parse().ok()?,
        updated_at: get_str("updated_at")?.parse().ok()?,
    })
}

fn row_to_tool_pg(row: &sqlx::postgres::PgRow) -> Option<ToolResource> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();
    let enabled = row
        .try_get::<bool, _>("enabled")
        .ok()
        .or_else(|| row.try_get::<i64, _>("enabled").ok().map(|v| v != 0))
        .unwrap_or(true);

    Some(ToolResource {
        id: get_string("id")?,
        name: get_string("name")?,
        description: get_string("description")?,
        parameters: serde_json::from_str(&get_string("parameters")?).ok()?,
        handler: get_optional_string("handler"),
        enabled,
        secrets: get_optional_string("secrets")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        side_effect: get_optional_string("side_effect")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default(),
        http: get_optional_string("http_config")
            .and_then(|s| serde_json::from_str(&s).ok()),
        created_at: get_string("created_at")?.parse().ok()?,
        updated_at: get_string("updated_at")?.parse().ok()?,
    })
}

fn row_to_tool_execution_sqlite(row: &sqlx::sqlite::SqliteRow) -> Option<ToolExecution> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_str = |col: &str| row.try_get::<&str, _>(col).ok();
    let get_optional_str = |col: &str| row.try_get::<Option<&str>, _>(col).ok().flatten();

    Some(ToolExecution {
        id: get_string("id")?,
        tool_id: get_string("tool_id")?,
        started_at: get_str("started_at")?.parse().ok()?,
        completed_at: get_optional_str("completed_at").and_then(|s| s.parse().ok()),
        input: serde_json::from_str(get_str("input")?).ok()?,
        output: get_optional_str("output").map(|s| s.to_string()),
        status: serde_json::from_str(get_str("status")?).ok()?,
        error: get_optional_str("error").map(|s| s.to_string()),
    })
}

fn row_to_tool_execution_pg(row: &sqlx::postgres::PgRow) -> Option<ToolExecution> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();

    let started_at = get_string("started_at")?;
    let completed_at = get_optional_string("completed_at");
    let input = get_string("input")?;
    let status = get_string("status")?;

    Some(ToolExecution {
        id: get_string("id")?,
        tool_id: get_string("tool_id")?,
        started_at: started_at.parse().ok()?,
        completed_at: completed_at.as_deref().and_then(|s| s.parse().ok()),
        input: serde_json::from_str(&input).ok()?,
        output: get_optional_string("output"),
        status: serde_json::from_str(&status).ok()?,
        error: get_optional_string("error"),
    })
}

fn row_to_proposal_sqlite(row: &sqlx::sqlite::SqliteRow) -> Option<Proposal> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();

    Some(Proposal {
        id: get_string("id")?,
        action: serde_json::from_str(&get_string("action")?).ok()?,
        rationale: get_string("rationale")?,
        risk: serde_json::from_str(&get_string("risk")?).ok()?,
        status: serde_json::from_str(&get_string("status")?).ok()?,
        proposed_by: get_string("proposed_by")?,
        created_at: get_string("created_at")?.parse().ok()?,
        resolved_at: get_optional_string("resolved_at").and_then(|s| s.parse().ok()),
        result: get_optional_string("result"),
    })
}

fn row_to_proposal_pg(row: &sqlx::postgres::PgRow) -> Option<Proposal> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();

    Some(Proposal {
        id: get_string("id")?,
        action: serde_json::from_str(&get_string("action")?).ok()?,
        rationale: get_string("rationale")?,
        risk: serde_json::from_str(&get_string("risk")?).ok()?,
        status: serde_json::from_str(&get_string("status")?).ok()?,
        proposed_by: get_string("proposed_by")?,
        created_at: get_string("created_at")?.parse().ok()?,
        resolved_at: get_optional_string("resolved_at").and_then(|s| s.parse().ok()),
        result: get_optional_string("result"),
    })
}

fn row_to_script_sqlite(row: &sqlx::sqlite::SqliteRow) -> Option<ScriptDefinition> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();
    let get_i64 = |col: &str| row.try_get::<i64, _>(col).ok();

    Some(ScriptDefinition {
        id: get_string("id")?,
        name: get_string("name")?,
        description: get_optional_string("description"),
        handler: get_string("handler")?,
        schedule: get_optional_string("schedule"),
        enabled: get_i64("enabled").unwrap_or(1) != 0,
        created_at: get_string("created_at")?.parse().ok()?,
        updated_at: get_string("updated_at")?.parse().ok()?,
        last_run: get_optional_string("last_run").as_deref().and_then(|s| s.parse().ok()),
        run_count: get_i64("run_count").unwrap_or(0) as u64,
    })
}

fn row_to_script_execution_sqlite(row: &sqlx::sqlite::SqliteRow) -> Option<ScriptExecution> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();
    let get_optional_i32 = |col: &str| row.try_get::<Option<i32>, _>(col).ok().flatten();

    let status = get_string("status")?;

    Some(ScriptExecution {
        id: get_string("id")?,
        script_id: get_string("script_id")?,
        started_at: get_string("started_at")?.parse().ok()?,
        completed_at: get_optional_string("completed_at").as_deref().and_then(|s| s.parse().ok()),
        exit_code: get_optional_i32("exit_code"),
        output: get_optional_string("output"),
        stderr: get_optional_string("stderr"),
        status: serde_json::from_str(&status).ok()?,
        error: get_optional_string("error"),
        triggered_by: get_string("triggered_by").unwrap_or_else(|| "manual".to_string()),
    })
}

fn row_to_script_pg(row: &sqlx::postgres::PgRow) -> Option<ScriptDefinition> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();

    Some(ScriptDefinition {
        id: get_string("id")?,
        name: get_string("name")?,
        description: get_optional_string("description"),
        handler: get_string("handler")?,
        schedule: get_optional_string("schedule"),
        enabled: row.try_get::<bool, _>("enabled").ok()?,
        created_at: row.try_get::<chrono::DateTime<Utc>, _>("created_at").ok()?,
        updated_at: row.try_get::<chrono::DateTime<Utc>, _>("updated_at").ok()?,
        last_run: row.try_get::<Option<chrono::DateTime<Utc>>, _>("last_run").ok().flatten(),
        run_count: row.try_get::<i64, _>("run_count").ok().unwrap_or(0) as u64,
    })
}

fn row_to_script_execution_pg(row: &sqlx::postgres::PgRow) -> Option<ScriptExecution> {
    let get_string = |col: &str| row.try_get::<String, _>(col).ok();
    let get_optional_string = |col: &str| row.try_get::<Option<String>, _>(col).ok().flatten();

    let status = get_string("status")?;

    Some(ScriptExecution {
        id: get_string("id")?,
        script_id: get_string("script_id")?,
        started_at: row.try_get::<chrono::DateTime<Utc>, _>("started_at").ok()?,
        completed_at: row.try_get::<Option<chrono::DateTime<Utc>>, _>("completed_at").ok().flatten(),
        exit_code: row.try_get::<Option<i32>, _>("exit_code").ok().flatten(),
        output: get_optional_string("output"),
        stderr: get_optional_string("stderr"),
        status: serde_json::from_str(&status).ok()?,
        error: get_optional_string("error"),
        triggered_by: get_string("triggered_by").unwrap_or_else(|| "manual".to_string()),
    })
}
