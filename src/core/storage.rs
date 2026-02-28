use async_trait::async_trait;
use sqlx::{
    postgres::PgPoolOptions,
    sqlite::SqlitePoolOptions,
    Pool, Row, Sqlite, Postgres,
};
use std::path::Path;

use super::agent::{Agent, ExecutionResult, ToolExecution, ToolResource};
use super::error::Result;

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
                deep_agent_config TEXT,
                environment TEXT NOT NULL,
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
                deep_agent_config TEXT,
                environment TEXT NOT NULL,
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
                execution_mode, trigger, schedule, deep_agent_config, environment,
                status, created_at, updated_at, last_run, run_count
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
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
        .bind(serde_json::to_string(&agent.deep_agent_config)?)
        .bind(serde_json::to_string(&agent.environment)?)
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
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
                status = ?12,
                updated_at = ?13,
                last_run = ?14,
                run_count = ?15
            WHERE id = ?16
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
        .bind(serde_json::to_string(&agent.status)?)
        .bind(agent.updated_at.to_rfc3339())
        .bind(agent.last_run.map(|d| d.to_rfc3339()))
        .bind(agent.run_count as i64)
        .bind(&agent.id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete_agent(
        &self, id: &str
    ) -> Result<bool> {
        let result = sqlx::query("DELETE FROM agents WHERE id = ?1")
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
            FROM agents ORDER BY created_at DESC
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
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

    async fn create_tool(&self, tool: &ToolResource) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO tools (id, name, description, parameters, handler, enabled, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(&tool.id)
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(serde_json::to_string(&tool.parameters)?)
        .bind(&tool.handler)
        .bind(if tool.enabled { 1i64 } else { 0i64 })
        .bind(tool.created_at.to_rfc3339())
        .bind(tool.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_tool(&self, id: &str) -> Result<Option<ToolResource>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, parameters, handler, enabled, created_at, updated_at
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
            SELECT id, name, description, parameters, handler, enabled, created_at, updated_at
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
                updated_at = ?6
            WHERE id = ?7
            "#,
        )
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(serde_json::to_string(&tool.parameters)?)
        .bind(&tool.handler)
        .bind(if tool.enabled { 1i64 } else { 0i64 })
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
            SELECT id, name, description, parameters, handler, enabled, created_at, updated_at
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
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
                execution_mode, trigger, schedule, deep_agent_config, environment,
                status, created_at, updated_at, last_run, run_count
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
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
        .bind(serde_json::to_string(&agent.deep_agent_config)?)
        .bind(serde_json::to_string(&agent.environment)?)
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
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
                status = $12,
                updated_at = $13,
                last_run = $14,
                run_count = $15
            WHERE id = $16
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
        .bind(serde_json::to_string(&agent.status)?)
        .bind(agent.updated_at.to_rfc3339())
        .bind(agent.last_run.map(|d| d.to_rfc3339()))
        .bind(agent.run_count as i64)
        .bind(&agent.id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn delete_agent(
        &self, id: &str
    ) -> Result<bool> {
        let result = sqlx::query("DELETE FROM agents WHERE id = $1")
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
            FROM agents ORDER BY created_at DESC
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
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

    async fn create_tool(&self, tool: &ToolResource) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO tools (id, name, description, parameters, handler, enabled, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(&tool.id)
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(serde_json::to_string(&tool.parameters)?)
        .bind(&tool.handler)
        .bind(tool.enabled)
        .bind(tool.created_at.to_rfc3339())
        .bind(tool.updated_at.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_tool(&self, id: &str) -> Result<Option<ToolResource>> {
        let row = sqlx::query(
            r#"
            SELECT id, name, description, parameters, handler, enabled, created_at, updated_at
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
            SELECT id, name, description, parameters, handler, enabled, created_at, updated_at
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
                updated_at = $6
            WHERE id = $7
            "#,
        )
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(serde_json::to_string(&tool.parameters)?)
        .bind(&tool.handler)
        .bind(tool.enabled)
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
            SELECT id, name, description, parameters, handler, enabled, created_at, updated_at
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
                   execution_mode, trigger, schedule, deep_agent_config, environment,
                   status, created_at, updated_at, last_run, run_count
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
        deep_agent_config: get_optional_str("deep_agent_config").and_then(|s| serde_json::from_str(s).ok()),
        environment: serde_json::from_str(get_str("environment")?).ok()?,
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
        deep_agent_config: deep_agent_config
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        environment: serde_json::from_str(&environment).ok()?,
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
