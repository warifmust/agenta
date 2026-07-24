use std::collections::HashMap;
use std::sync::Arc;
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use agenta::core::{
    Agent, AgentStatus, ExecutionMode, ExecutionResult, Storage, ToolExecution,
    ToolExecutionStatus, ToolResource, TriggerEvent, TriggerType,
    ScriptDefinition, ScriptExecution, ScriptExecutionStatus,
    Proposal, ProposalAction, ProposalStatus,
};
use agenta::core::AppConfig;
use agenta::providers::build_backend;
use agenta::scheduler::AgentExecutor;
use agenta::trigger::{CommandTrigger, FileWatcherTrigger, HttpTrigger, Scheduler as CronScheduler, resolve_timezone};

pub struct DaemonState {
    storage: Arc<dyn Storage>,
    executor: AgentExecutor,
    config: AppConfig,
    cron_scheduler: CronScheduler,
    running_agents: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    http_trigger: Arc<RwLock<Option<HttpTrigger>>>,
    file_watcher: Arc<RwLock<Option<FileWatcherTrigger>>>,
    command_triggers: Arc<RwLock<HashMap<String, CommandTrigger>>>,
    event_sender: Arc<RwLock<Option<tokio::sync::mpsc::Sender<TriggerEvent>>>>,
}

impl DaemonState {
    pub async fn new(storage: Arc<dyn Storage>, config: &AppConfig) -> anyhow::Result<Self> {
        let backend = build_backend(config, None);
        let executor = AgentExecutor::new(storage.clone(), backend);
        let timezone = resolve_timezone(config.timezone.as_deref());
        let cron_scheduler = CronScheduler::with_timezone(timezone);

        Ok(Self {
            storage,
            executor,
            config: config.clone(),
            cron_scheduler,
            running_agents: Arc::new(RwLock::new(HashMap::new())),
            http_trigger: Arc::new(RwLock::new(None)),
            file_watcher: Arc::new(RwLock::new(None)),
            command_triggers: Arc::new(RwLock::new(HashMap::new())),
            event_sender: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn start_background_tasks(&self) -> anyhow::Result<()> {
        // Reconcile runs orphaned by a previous daemon exit (crash or restart).
        // A fresh daemon owns no running tasks, so anything still "running" was
        // interrupted — cancel it and return its agent to active, so it doesn't
        // sit as a permanent "running" ghost.
        match self.storage.reconcile_interrupted_runs().await {
            Ok(0) => {}
            Ok(n) => info!("Reconciled {} interrupted run(s) from a previous daemon", n),
            Err(e) => warn!("Startup run reconciliation failed: {}", e),
        }

        // Start cron scheduler
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        *self.event_sender.write().await = Some(tx.clone());
        self.cron_scheduler.start(tx.clone()).await;

        // Load scheduled agents
        match self.storage.get_scheduled_agents().await {
            Ok(agents) => {
                for agent in agents {
                    if let Some(_) = &agent.schedule {
                        if let Err(e) = self.cron_scheduler.add_job(&agent).await {
                            warn!("Failed to schedule agent {}: {}", agent.name, e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Failed to load scheduled agents: {}", e);
            }
        }

        // Setup HTTP trigger
        let mut http_trigger = HttpTrigger::new();
        http_trigger.start(8787, tx.clone()).await?;
        *self.http_trigger.write().await = Some(http_trigger);

        // Setup file watcher
        let mut file_watcher = FileWatcherTrigger::new();
        file_watcher
            .watch_path(
                std::path::Path::new("."),
                true,
                tx.clone(),
            )
            .await?;
        *self.file_watcher.write().await = Some(file_watcher);

        // Load triggered agents (HTTP + command)
        match self.storage.get_triggered_agents().await {
            Ok(agents) => {
                for agent in agents {
                    if let Err(e) = self.register_triggers_for_agent(&agent).await {
                        warn!("Failed to register triggers for {}: {}", agent.name, e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to load triggered agents: {}", e);
            }
        }

        // Handle trigger events. Each run builds an executor from the agent's own
        // provider (via build_executor_for_agent) rather than the shared default —
        // otherwise every scheduled/triggered run would hit the default backend
        // (Ollama) and a deepseek/openai agent would 404 on its own cron.
        let storage = self.storage.clone();
        let executor = self.executor.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    TriggerEvent::Scheduled { agent_id, cron } => {
                        info!("Scheduled trigger for {}: {}", agent_id, cron);
                        if let Ok(Some(agent)) = storage.get_agent(&agent_id).await {
                            let ex = build_executor_for_agent(&config, &storage, &executor, &agent);
                            // Pass the agent's scheduled directive as the run input so the
                            // model knows what this tick is for; empty input makes it freewheel.
                            let _ = ex.execute(&agent, agent.scheduled_input.clone()).await;
                        }
                    }
                    TriggerEvent::FileCreated { path } => {
                        info!("File created trigger: {}", path);
                        // Handle file triggers
                        if let Ok(agents) = storage.get_triggered_agents().await {
                            for agent in agents {
                                if let Some(TriggerType::FileWatcher { path: watch_path, .. }) =
                                    &agent.trigger
                                {
                                    if path.contains(watch_path) {
                                        let ex = build_executor_for_agent(&config, &storage, &executor, &agent);
                                        let _ = ex.execute(&agent, Some(path.clone())).await;
                                    }
                                }
                            }
                        }
                    }
                    TriggerEvent::HttpRequest { agent_id, method, path, body } => {
                        info!("HTTP trigger: {} {} -> {}", method, path, agent_id);
                        if let Ok(Some(agent)) = storage.get_agent(&agent_id).await {
                            let input = body.unwrap_or_default();
                            let ex = build_executor_for_agent(&config, &storage, &executor, &agent);
                            let _ = ex.execute(&agent, Some(input)).await;
                        }
                    }
                    TriggerEvent::CommandOutput { agent_id, command, output, matched: _ } => {
                        info!("Command trigger: {} -> {}", command, agent_id);
                        if let Ok(Some(agent)) = storage.get_agent(&agent_id).await {
                            let ex = build_executor_for_agent(&config, &storage, &executor, &agent);
                            let _ = ex.execute(&agent, Some(output)).await;
                        }
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }

    pub fn storage(&self) -> Arc<dyn Storage> {
        self.storage.clone()
    }

    pub async fn create_agent(&self,
        mut agent: Agent,
    ) -> anyhow::Result<String> {
        agent.status = AgentStatus::Active;

        // Validate the cron schedule BEFORE persisting. Scheduling happens after
        // the insert below, so a bad expression there would fail *after* the agent
        // was already committed — leaving an orphan agent with an unusable schedule.
        if let ExecutionMode::Scheduled = agent.execution_mode {
            if let Some(expr) = &agent.schedule {
                expr.parse::<cron::Schedule>()
                    .map_err(|e| anyhow::anyhow!("Invalid cron expression: {}", e))?;
            }
        }

        self.storage.create_agent(&agent).await?;

        // Schedule if needed
        if let ExecutionMode::Scheduled = agent.execution_mode {
            if let Some(_) = &agent.schedule {
                self.cron_scheduler.add_job(&agent).await?;
            }
        }

        if let Err(e) = self.register_triggers_for_agent(&agent).await {
            warn!("Failed to register triggers for {}: {}", agent.name, e);
        }

        Ok(agent.id.clone())
    }

    pub async fn update_agent(
        &self,
        id: String,
        mut agent: Agent,
    ) -> anyhow::Result<()> {
        let previous = self.storage.get_agent(&id).await?;

        agent.id = id;
        agent.touch();
        self.storage.update_agent(&agent).await?;

        if let Some(prev) = previous {
            self.unregister_triggers_for_agent(&prev).await;
        }
        if let Err(e) = self.register_triggers_for_agent(&agent).await {
            warn!("Failed to register triggers for {}: {}", agent.name, e);
        }

        Ok(())
    }

    pub async fn delete_agent(&self,
        id_or_name: &str,
    ) -> anyhow::Result<bool> {
        let Some(agent) = self.get_agent(id_or_name).await? else {
            return Ok(false);
        };
        let id = agent.id;

        // Stop if running
        self.stop_agent(&id).await?;

        // Remove from scheduler
        self.cron_scheduler.remove_job(&id).await;

        self.unregister_triggers_by_id(&id).await;

        Ok(self.storage.delete_agent(&id).await?)
    }

    pub async fn get_agent(&self,
        id: &str,
    ) -> anyhow::Result<Option<Agent>> {
        let mut found = match self.storage.get_agent(id).await? {
            Some(agent) => Some(agent),
            None => self.storage.get_agent_by_name(id).await?,
        };
        // MIND's prompt is binary-sourced — surface the compiled-in version, not the
        // vestigial DB copy, so `agenta get MIND` and runs agree and reflect upgrades.
        if let Some(agent) = found.as_mut() {
            if agenta::core::is_mind(agent) {
                agent.system_prompt = agenta::core::MIND_SYSTEM_PROMPT.to_string();
            }
        }
        Ok(found)
    }

    pub async fn list_agents(&self,
    ) -> anyhow::Result<Vec<Agent>> {
        Ok(self.storage.list_agents().await?)
    }

    /// Build an executor using the agent's provider override (if any), falling back to default.
    fn executor_for_agent(&self, agent: &Agent) -> AgentExecutor {
        build_executor_for_agent(&self.config, &self.storage, &self.executor, agent)
    }

    /// RAG auto-inject: if the agent has knowledge bases, retrieve passages relevant
    /// to `input` and append them (with citations) to a working copy of the system
    /// prompt. Retrieval failures are logged, never fatal to the run.
    async fn inject_knowledge(&self, agent: &mut Agent, input: &str) {
        if agent.config.knowledge_bases.is_empty() || input.trim().is_empty() {
            return;
        }
        // top-k passages to inject: per-agent override (`--top-k`) if set, else the
        // global `rag_top_k` (default 8). Kept generous so a short query like "dua
        // during sujud" still pulls the content page even when several nearby
        // sections rank alongside it.
        let top_k = agent.config.rag_top_k.unwrap_or(self.config.rag_top_k);
        match agenta::knowledge::retrieve_context(
            &self.config,
            &agent.config.knowledge_bases,
            input,
            top_k,
        )
        .await
        {
            Ok(Some(block)) => {
                agent.system_prompt = format!("{}\n\n{}", agent.system_prompt, block);
            }
            Ok(None) => {}
            Err(e) => warn!("Knowledge retrieval failed for agent {}: {}", agent.name, e),
        }
    }

    pub async fn run_agent(
        &self,
        id: &str,
        input: Option<String>,
    ) -> anyhow::Result<String> {
        let mut agent = self
            .get_agent(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found"))?;

        if let Some(inp) = input.as_deref() {
            self.inject_knowledge(&mut agent, inp).await;
        }

        let _storage = self.storage.clone();
        let executor = self.executor_for_agent(&agent);

        let execution_id = uuid::Uuid::new_v4().to_string();
        let execution_id_clone = execution_id.clone();
        let agent_id = agent.id.clone();
        let agent_clone = agent.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = executor.execute_with_id(&agent_clone, input, execution_id_clone).await {
                error!("Agent execution failed: {}", e);
            }
        });

        self.running_agents
            .write()
            .await
            .insert(agent_id, handle);

        Ok(execution_id)
    }

    #[allow(dead_code)]
    pub async fn run_agent_sync(
        &self,
        id: &str,
        input: String,
    ) -> anyhow::Result<String> {
        let execution = self.run_agent_sync_execution(id, input).await?;
        match execution.output {
            Some(output) => Ok(output),
            None => Err(anyhow::anyhow!(
                "Execution completed without output"
            )),
        }
    }

    #[allow(dead_code)]
    pub async fn run_agent_sync_execution(
        &self,
        id: &str,
        input: String,
    ) -> anyhow::Result<ExecutionResult> {
        let mut agent = self
            .get_agent(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found"))?;

        self.inject_knowledge(&mut agent, &input).await;

        let execution_id = uuid::Uuid::new_v4().to_string();
        self
            .executor_for_agent(&agent)
            .execute_with_id(&agent, Some(input), execution_id)
            .await
    }

    /// Same as `run_agent_sync_execution` but attaches a progress channel.
    /// Progress messages (e.g. sub-agent notifications) are sent through `progress_tx`
    /// while the execution is running.
    pub async fn run_agent_sync_execution_with_progress(
        &self,
        id: &str,
        input: String,
        progress_tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> anyhow::Result<ExecutionResult> {
        let mut agent = self
            .get_agent(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found"))?;

        self.inject_knowledge(&mut agent, &input).await;

        let execution_id = uuid::Uuid::new_v4().to_string();
        self.executor_for_agent(&agent)
            .with_progress(progress_tx)
            .execute_with_id(&agent, Some(input), execution_id)
            .await
    }

    pub async fn stop_agent(
        &self,
        id: &str,
    ) -> anyhow::Result<()> {
        let agent_id = if let Some(agent) = self.get_agent(id).await? {
            agent.id
        } else {
            id.to_string()
        };

        let mut running = self.running_agents.write().await;
        if let Some(handle) = running.remove(&agent_id) {
            handle.abort();
            info!("Stopped agent: {}", agent_id);
        }

        // Mark all running executions as cancelled in the DB
        if let Err(e) = self.storage.cancel_running_executions(&agent_id).await {
            warn!("Failed to cancel running executions for agent {}: {}", agent_id, e);
        }

        Ok(())
    }

    pub async fn get_logs(
        &self,
        agent_id: &str,
        execution_id: Option<&str>,
        lines: usize,
    ) -> anyhow::Result<Vec<String>> {
        let resolved_agent_id = if let Some(agent) = self.get_agent(agent_id).await? {
            agent.id
        } else {
            agent_id.to_string()
        };

        if let Some(exec_id) = execution_id {
            // Accept either a full id or the short prefix that list-mode prints
            // (`exec.id[..8]`). Try an exact match first, then fall back to a
            // prefix match among the agent's recent executions — so the ids shown
            // by `agenta logs <agent>` actually work with `-e`.
            let execution = match self.storage.get_execution(exec_id).await? {
                Some(e) => Some(e),
                None => self
                    .storage
                    .list_executions(&resolved_agent_id, 200)
                    .await?
                    .into_iter()
                    .find(|e| e.id.starts_with(exec_id)),
            };
            if let Some(execution) = execution {
                let mut logs = Vec::new();
                logs.push(format!("Execution: {}", execution.id));
                logs.push(format!("Started: {}", execution.started_at));
                if let Some(completed) = execution.completed_at {
                    logs.push(format!("Completed: {}", completed));
                }
                logs.push(format!("Status: {:?}", execution.status));
                if let Some(error) = &execution.error {
                    logs.push(format!("Error: {}", error));
                }
                if let Some(output) = &execution.output {
                    logs.push(format!("Output:\n{}", output));
                }
                Ok(logs)
            } else {
                Ok(vec!["Execution not found".to_string()])
            }
        } else {
            // Get last N executions
            let executions = self.storage.list_executions(&resolved_agent_id, lines as i64).await?;
            let mut logs = Vec::new();
            for exec in executions {
                logs.push(format!(
                    "[{}] {} - {:?}",
                    exec.started_at.format("%Y-%m-%d %H:%M:%S"),
                    exec.id[..8].to_string(),
                    exec.status
                ));
            }
            Ok(logs)
        }
    }

    pub async fn get_execution(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<ExecutionResult>> {
        Ok(self.storage.get_execution(id).await?)
    }

    pub async fn list_executions_for_agent(
        &self,
        agent_id_or_name: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<ExecutionResult>> {
        let resolved_agent_id = if let Some(agent) = self.get_agent(agent_id_or_name).await? {
            agent.id
        } else {
            agent_id_or_name.to_string()
        };
        self.storage
            .list_executions(&resolved_agent_id, limit)
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn list_executions(&self, limit: i64) -> anyhow::Result<Vec<ExecutionResult>> {
        let agents = self.storage.list_agents().await?;
        let mut all = Vec::new();

        for agent in agents {
            let mut executions = self.storage.list_executions(&agent.id, limit).await?;
            all.append(&mut executions);
        }

        all.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        all.truncate(limit.max(0) as usize);
        Ok(all)
    }

    pub async fn stop_all(&self) {
        // Stop all running agents
        let mut running = self.running_agents.write().await;
        for (id, handle) in running.drain() {
            handle.abort();
            info!("Stopped agent: {}", id);
        }

        // Stop cron scheduler
        self.cron_scheduler.stop().await;

        // Stop HTTP trigger
        if let Some(trigger) = self.http_trigger.write().await.take() {
            drop(trigger);
        }

        // Stop file watcher
        if let Some(mut watcher) = self.file_watcher.write().await.take() {
            let _ = watcher.stop();
        }

        // Stop command triggers
        let mut triggers = self.command_triggers.write().await;
        for (_id, trigger) in triggers.drain() {
            let _ = trigger.stop().await;
        }
    }

    pub async fn create_tool(&self, mut tool: ToolResource) -> anyhow::Result<String> {
        tool.updated_at = chrono::Utc::now();
        self.storage.create_tool(&tool).await?;
        Ok(tool.id.clone())
    }

    pub async fn get_tool(&self, id_or_name: &str) -> anyhow::Result<Option<ToolResource>> {
        if let Some(tool) = self.storage.get_tool(id_or_name).await? {
            return Ok(Some(tool));
        }
        self.storage
            .get_tool_by_name(id_or_name)
            .await
            .map_err(anyhow::Error::from)
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<ToolResource>> {
        self.storage.list_tools().await.map_err(anyhow::Error::from)
    }

    // ── Knowledge bases (RAG) ─────────────────────────────────────────────────
    // KBs live in Postgres/pgvector, not the agent store — queried directly, same
    // as the `agenta knowledge` CLI. Ingestion is intentionally NOT here (it needs
    // an async upload+OCR job); this is lifecycle only.
    async fn kb_store(&self) -> anyhow::Result<agenta::knowledge::PgVectorStore> {
        let url = match &self.config.database_url {
            Some(u) if u.starts_with("postgres") => u.clone(),
            _ => anyhow::bail!("RAG requires Postgres. Set database_url in config.toml."),
        };
        Ok(agenta::knowledge::PgVectorStore::new(&url).await?)
    }

    pub async fn list_kbs(&self) -> anyhow::Result<Vec<agenta::knowledge::KnowledgeBase>> {
        use agenta::knowledge::VectorStore;
        Ok(self.kb_store().await?.list_kbs().await?)
    }

    pub async fn get_kb(&self, name: &str) -> anyhow::Result<Option<agenta::knowledge::KnowledgeBase>> {
        use agenta::knowledge::VectorStore;
        Ok(self.kb_store().await?.get_kb(name).await?)
    }

    pub async fn create_kb(
        &self,
        name: &str,
        embedder: &str,
    ) -> anyhow::Result<agenta::knowledge::KnowledgeBase> {
        use agenta::knowledge::VectorStore;
        let store = self.kb_store().await?;
        if store.get_kb(name).await?.is_some() {
            anyhow::bail!("Knowledge base '{}' already exists", name);
        }
        // Validate the embedder is usable and produces the pinned dimension.
        agenta::providers::ensure_embedder_available(&self.config, embedder).await?;
        let emb = agenta::providers::build_embedder(&self.config, embedder).await?;
        let dim = emb.dimension() as i32;
        if dim != agenta::knowledge::V1_DIMENSION {
            anyhow::bail!(
                "v1 supports {}-dim embedders only; '{}' is {}-dim",
                agenta::knowledge::V1_DIMENSION,
                embedder,
                dim
            );
        }
        Ok(store.create_kb(name, &emb.id(), dim).await?)
    }

    pub async fn delete_kb(&self, name: &str) -> anyhow::Result<bool> {
        use agenta::knowledge::VectorStore;
        Ok(self.kb_store().await?.delete_kb(name).await?)
    }

    pub async fn update_tool(&self, id_or_name: &str, mut tool: ToolResource) -> anyhow::Result<()> {
        let existing = self
            .get_tool(id_or_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Tool not found"))?;
        let previous_name = existing.name.clone();
        tool.id = existing.id;
        tool.created_at = existing.created_at;
        tool.updated_at = chrono::Utc::now();
        self.storage.update_tool(&tool).await?;
        // Agents carry their own copy of each tool, so updating the registry alone
        // leaves them running the old definition — the update looks applied but
        // changes nothing where it matters. Push it out to them too.
        self.sync_tool_to_agents(&previous_name, &tool).await?;
        Ok(())
    }

    /// Refresh every agent's embedded copy of `tool` (matched by its previous
    /// name, so a rename still finds them). Returns how many agents were updated.
    pub async fn sync_tool_to_agents(
        &self,
        previous_name: &str,
        tool: &ToolResource,
    ) -> anyhow::Result<usize> {
        let agents = self.storage.list_agents().await?;
        let mut updated = 0usize;
        for mut agent in agents {
            let mut touched = false;
            for slot in agent.tools.iter_mut() {
                if slot.name == previous_name {
                    let mut next = tool.as_definition();
                    // The registry value is the base (as_definition carries it), but a
                    // per-agent override still wins — so a sync can't clobber e.g.
                    // ACE's run_pipeline 900s timeout, while a registry-level timeout
                    // still reaches agents that never set their own.
                    if slot.timeout_secs.is_some() {
                        next.timeout_secs = slot.timeout_secs;
                    }
                    if !slot.requires.is_empty() {
                        next.requires = slot.requires.clone();
                    }
                    *slot = next;
                    touched = true;
                }
            }
            if touched {
                self.storage.update_agent(&agent).await?;
                updated += 1;
            }
        }
        Ok(updated)
    }

    pub async fn delete_tool(&self, id_or_name: &str) -> anyhow::Result<bool> {
        let Some(tool) = self.get_tool(id_or_name).await? else {
            return Ok(false);
        };
        self.storage.delete_tool(&tool.id).await.map_err(anyhow::Error::from)
    }

    pub async fn run_tool(
        &self,
        id_or_name: &str,
        input: serde_json::Value,
    ) -> anyhow::Result<String> {
        let tool = self
            .get_tool(id_or_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Tool not found"))?;

        if !tool.enabled {
            return Err(anyhow::anyhow!("Tool is disabled"));
        }

        let mut execution = ToolExecution::new(tool.id.clone(), input.clone());
        execution.status = ToolExecutionStatus::Running;
        self.storage.create_tool_execution(&execution).await?;

        let result = run_tool_handler(&tool, input).await;
        match result {
            Ok(output) => {
                execution.output = Some(output.clone());
                execution.completed_at = Some(chrono::Utc::now());
                execution.status = ToolExecutionStatus::Completed;
                self.storage.update_tool_execution(&execution).await?;
                Ok(execution.id)
            }
            Err(err) => {
                execution.completed_at = Some(chrono::Utc::now());
                execution.status = ToolExecutionStatus::Failed;
                execution.error = Some(err.to_string());
                self.storage.update_tool_execution(&execution).await?;
                Ok(execution.id)
            }
        }
    }

    pub async fn get_tool_execution(&self, id: &str) -> anyhow::Result<Option<ToolExecution>> {
        self.storage.get_tool_execution(id).await.map_err(anyhow::Error::from)
    }

    pub async fn get_tool_logs(
        &self,
        id_or_name: &str,
        execution_id: Option<&str>,
        lines: usize,
    ) -> anyhow::Result<Vec<String>> {
        let tool = self
            .get_tool(id_or_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Tool not found"))?;

        if let Some(exec_id) = execution_id {
            if let Some(execution) = self.storage.get_tool_execution(exec_id).await? {
                let mut logs = vec![
                    format!("Tool execution: {}", execution.id),
                    format!("Started: {}", execution.started_at),
                    format!("Status: {:?}", execution.status),
                ];
                if let Some(completed) = execution.completed_at {
                    logs.push(format!("Completed: {}", completed));
                }
                if let Some(error) = execution.error {
                    logs.push(format!("Error: {}", error));
                }
                if let Some(output) = execution.output {
                    logs.push(format!("Output:\n{}", output));
                }
                return Ok(logs);
            }
            return Ok(vec!["Tool execution not found".to_string()]);
        }

        let executions = self
            .storage
            .list_tool_executions(&tool.id, lines as i64)
            .await?;
        Ok(executions
            .into_iter()
            .map(|e| {
                format!(
                    "[{}] {} - {:?}",
                    e.started_at.format("%Y-%m-%d %H:%M:%S"),
                    &e.id[..8],
                    e.status
                )
            })
            .collect())
    }

    // ── Agent memories (corrective feedback / preferences) ───────────────────

    pub async fn add_memory(&self, scope: &str, kind: &str, content: &str) -> anyhow::Result<()> {
        let mem = agenta::core::Memory::new(scope, kind, content);
        self.storage.add_memory(&mem).await.map_err(anyhow::Error::from)
    }

    pub async fn list_memories(
        &self,
        scope: &str,
        active_only: bool,
    ) -> anyhow::Result<Vec<agenta::core::Memory>> {
        self.storage.list_memories(scope, active_only).await.map_err(anyhow::Error::from)
    }

    pub async fn delete_memory(&self, id: &str) -> anyhow::Result<bool> {
        self.storage.delete_memory(id).await.map_err(anyhow::Error::from)
    }

    // ── Proposals (human-gated mutations from agents like MIND) ──────────────

    pub async fn create_proposal(&self, proposal: &Proposal) -> anyhow::Result<()> {
        self.storage.create_proposal(proposal).await.map_err(anyhow::Error::from)
    }

    pub async fn list_proposals(
        &self,
        status: Option<ProposalStatus>,
    ) -> anyhow::Result<Vec<Proposal>> {
        self.storage.list_proposals(status).await.map_err(anyhow::Error::from)
    }

    pub async fn get_proposal(&self, id: &str) -> anyhow::Result<Option<Proposal>> {
        self.storage.get_proposal(id).await.map_err(anyhow::Error::from)
    }

    /// Approve and apply a pending proposal. Runs the underlying CRUD, then
    /// records the outcome. Returns the (updated) proposal.
    pub async fn approve_proposal(&self, id: &str) -> anyhow::Result<Proposal> {
        let mut proposal = self
            .get_proposal(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Proposal not found: {}", id))?;

        if proposal.status != ProposalStatus::Pending {
            return Err(anyhow::anyhow!(
                "Proposal {} is already {:?}",
                &proposal.id[..8],
                proposal.status
            ));
        }

        let outcome = self.apply_proposal_action(&proposal.action).await;
        proposal.resolved_at = Some(chrono::Utc::now());
        match outcome {
            Ok(msg) => {
                proposal.status = ProposalStatus::Applied;
                proposal.result = Some(msg);
            }
            Err(err) => {
                proposal.status = ProposalStatus::Failed;
                proposal.result = Some(err.to_string());
            }
        }
        self.storage.update_proposal(&proposal).await?;
        Ok(proposal)
    }

    /// Reject a pending proposal without applying it.
    pub async fn reject_proposal(&self, id: &str, reason: Option<String>) -> anyhow::Result<Proposal> {
        let mut proposal = self
            .get_proposal(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Proposal not found: {}", id))?;

        if proposal.status != ProposalStatus::Pending {
            return Err(anyhow::anyhow!(
                "Proposal {} is already {:?}",
                &proposal.id[..8],
                proposal.status
            ));
        }

        proposal.status = ProposalStatus::Rejected;
        proposal.resolved_at = Some(chrono::Utc::now());

        // Failure memory (AQL §16): a rejection WITH a reason becomes a memory for
        // the proposer (e.g. MIND), so it won't re-propose the same thing. No reason
        // = no signal = no memory (avoids noise). Best-effort — never fail the reject.
        let why = reason
            .as_ref()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty());
        proposal.result = reason.or(Some("rejected by user".to_string()));
        self.storage.update_proposal(&proposal).await?;

        if let Some(why) = why {
            let content = format!(
                "You previously proposed: {} — the user REJECTED it. Reason: {}. Don't propose this again unless they ask.",
                proposal.summary(),
                why
            );
            let mem = agenta::core::Memory::new(proposal.proposed_by.clone(), "rejection", content);
            if let Err(e) = self.storage.add_memory(&mem).await {
                warn!("Failed to store rejection memory: {}", e);
            }
        }
        Ok(proposal)
    }

    /// Execute the mutation a proposal describes, reusing the existing CRUD.
    async fn apply_proposal_action(&self, action: &ProposalAction) -> anyhow::Result<String> {
        match action {
            ProposalAction::CreateTool(tool) => {
                if self.storage.get_tool_by_name(&tool.name).await?.is_some() {
                    return Err(anyhow::anyhow!("A tool named '{}' already exists", tool.name));
                }
                self.storage.create_tool(tool).await?;
                Ok(format!("Created tool '{}'", tool.name))
            }
            ProposalAction::CreateAgent(agent) => {
                if self.storage.get_agent_by_name(&agent.name).await?.is_some() {
                    return Err(anyhow::anyhow!("An agent named '{}' already exists", agent.name));
                }
                self.storage.create_agent(agent).await?;
                Ok(format!("Created agent '{}'", agent.name))
            }
            ProposalAction::UpdateTool { previous_name, tool } => {
                let existing = self
                    .get_tool(previous_name)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Tool '{}' not found", previous_name))?;
                let mut next = tool.clone();
                next.id = existing.id;
                next.created_at = existing.created_at;
                next.updated_at = chrono::Utc::now();
                self.storage.update_tool(&next).await?;
                let synced = self.sync_tool_to_agents(previous_name, &next).await?;
                Ok(format!(
                    "Updated tool '{}'{}",
                    next.name,
                    match synced {
                        0 => String::new(),
                        n => format!(" (refreshed in {} agent(s))", n),
                    }
                ))
            }
            ProposalAction::UpdateAgent {
                agent,
                system_prompt,
                description,
                model,
            } => {
                let mut a = self
                    .storage
                    .get_agent_by_name(agent)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent))?;

                let mut changed: Vec<&str> = Vec::new();
                if let Some(p) = system_prompt {
                    if p.trim().is_empty() {
                        return Err(anyhow::anyhow!(
                            "Refusing to set an empty system prompt on '{}'",
                            agent
                        ));
                    }
                    a.system_prompt = p.clone();
                    changed.push("system prompt");
                }
                if let Some(d) = description {
                    a.description = Some(d.clone());
                    changed.push("description");
                }
                if let Some(m) = model {
                    a.model = m.clone();
                    changed.push("model");
                }
                if changed.is_empty() {
                    return Ok(format!("Nothing to change on '{}'", agent));
                }
                self.storage.update_agent(&a).await?;
                Ok(format!("Updated {} on '{}'", changed.join(" + "), agent))
            }
            ProposalAction::AttachKb { agent, kb } => {
                let mut a = self
                    .storage
                    .get_agent_by_name(agent)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent))?;
                if self.get_kb(kb).await?.is_none() {
                    return Err(anyhow::anyhow!("Knowledge base '{}' not found", kb));
                }
                if a.config.knowledge_bases.iter().any(|k| k == kb) {
                    return Ok(format!("'{}' already has knowledge base '{}'", agent, kb));
                }
                a.config.knowledge_bases.push(kb.clone());
                self.storage.update_agent(&a).await?;
                Ok(format!("Attached knowledge base '{}' to '{}'", kb, agent))
            }
            ProposalAction::DetachKb { agent, kb } => {
                let mut a = self
                    .storage
                    .get_agent_by_name(agent)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found", agent))?;
                let before = a.config.knowledge_bases.len();
                a.config.knowledge_bases.retain(|k| k != kb);
                if a.config.knowledge_bases.len() == before {
                    return Ok(format!("'{}' did not have knowledge base '{}'", agent, kb));
                }
                self.storage.update_agent(&a).await?;
                Ok(format!("Detached knowledge base '{}' from '{}'", kb, agent))
            }
        }
    }
}

/// Build an executor honoring the agent's provider override, falling back to the
/// shared default executor when the agent has no (non-empty) provider set. Used
/// by both the request path (`run_agent`) and the trigger loop, so scheduled/
/// file/http/command runs route to the same backend a manual run would — e.g. a
/// `deepseek` agent must not silently fall through to Ollama on its cron.
fn build_executor_for_agent(
    config: &AppConfig,
    storage: &Arc<dyn Storage>,
    fallback: &AgentExecutor,
    agent: &Agent,
) -> AgentExecutor {
    let has_provider = agent
        .provider
        .as_deref()
        .map(|p| !p.trim().is_empty())
        .unwrap_or(false);
    if has_provider {
        let backend = build_backend(config, agent.provider.as_deref());
        AgentExecutor::new(storage.clone(), backend)
    } else {
        fallback.clone()
    }
}

async fn run_tool_handler(
    tool: &ToolResource,
    parameters: serde_json::Value,
) -> anyhow::Result<String> {
    // Delegate to the single shared executor so the manual `tool run` path gets
    // the same validation, secret allowlist, env sealing, and timeout as agent runs.
    agenta::tools::execute_tool(&tool.as_definition(), parameters).await
}

impl DaemonState {
    async fn register_triggers_for_agent(&self, agent: &Agent) -> anyhow::Result<()> {
        match &agent.trigger {
            Some(TriggerType::HttpWebhook { port, path, method }) => {
                if *port != 8787 {
                    warn!(
                        "Agent {} requested port {}, but server is on 8787",
                        agent.name, port
                    );
                }
                if let Some(trigger) = self.http_trigger.read().await.as_ref() {
                    trigger
                        .register_webhook(path.clone(), method.clone(), agent.id.clone())
                        .await;
                }
            }
            Some(TriggerType::CommandTrigger { command, interval_seconds, condition }) => {
                let sender = self.event_sender.read().await.clone();
                let Some(event_sender) = sender else {
                    return Err(anyhow::anyhow!("Trigger channel not initialized"));
                };

                let trigger = CommandTrigger::new();
                trigger
                    .start_monitoring(
                        agent.id.clone(),
                        command.clone(),
                        condition.clone(),
                        *interval_seconds,
                        event_sender,
                    )
                    .await?;
                self.command_triggers
                    .write()
                    .await
                    .insert(agent.id.clone(), trigger);
            }
            _ => {}
        }

        Ok(())
    }

    async fn unregister_triggers_for_agent(&self, agent: &Agent) {
        self.unregister_triggers_by_id(&agent.id).await;
    }

    async fn unregister_triggers_by_id(&self, agent_id: &str) {
        if let Some(trigger) = self.command_triggers.write().await.remove(agent_id) {
            let _ = trigger.stop().await;
        }

        if let Some(trigger) = self.http_trigger.read().await.as_ref() {
            trigger.unregister_webhook(agent_id).await;
        }
    }

    // ── Script management ─────────────────────────────────────────────────────

    pub async fn create_script(&self, script: ScriptDefinition) -> anyhow::Result<String> {
        let id = script.id.clone();
        self.storage.create_script(&script).await?;
        info!("Created script: {} ({})", script.name, id);
        Ok(id)
    }

    pub async fn get_script(&self, id_or_name: &str) -> anyhow::Result<Option<ScriptDefinition>> {
        Ok(self.storage.get_script(id_or_name).await?)
    }

    pub async fn list_scripts(&self) -> anyhow::Result<Vec<ScriptDefinition>> {
        Ok(self.storage.list_scripts().await?)
    }

    pub async fn update_script(&self, id_or_name: &str, mut script: ScriptDefinition) -> anyhow::Result<()> {
        let existing = self.storage.get_script(id_or_name).await?
            .ok_or_else(|| anyhow::anyhow!("Script not found: {}", id_or_name))?;
        script.id = existing.id;
        script.created_at = existing.created_at;
        script.run_count = existing.run_count;
        script.last_run = existing.last_run;
        script.touch();
        self.storage.update_script(&script).await?;
        Ok(())
    }

    pub async fn delete_script(&self, id_or_name: &str) -> anyhow::Result<bool> {
        Ok(self.storage.delete_script(id_or_name).await?)
    }

    pub async fn run_script(&self, id_or_name: &str) -> anyhow::Result<String> {
        let script = self.storage.get_script(id_or_name).await?
            .ok_or_else(|| anyhow::anyhow!("Script not found: {}", id_or_name))?;

        let mut execution = ScriptExecution::new(script.id.clone(), "manual");
        execution.status = ScriptExecutionStatus::Running;
        self.storage.create_script_execution(&execution).await?;

        let storage = self.storage.clone();
        let handler = script.handler.clone();
        let script_id = script.id.clone();
        let exec_id = execution.id.clone();

        tokio::spawn(async move {
            let result = run_script_handler(&handler).await;
            let mut exec = execution;
            exec.completed_at = Some(chrono::Utc::now());
            match result {
                Ok((stdout, stderr, exit_code)) => {
                    exec.output = Some(stdout);
                    exec.stderr = if stderr.is_empty() { None } else { Some(stderr) };
                    exec.exit_code = Some(exit_code);
                    exec.status = if exit_code == 0 {
                        ScriptExecutionStatus::Completed
                    } else {
                        ScriptExecutionStatus::Failed
                    };
                }
                Err(e) => {
                    exec.status = ScriptExecutionStatus::Failed;
                    exec.error = Some(e.to_string());
                }
            }
            let _ = storage.update_script_execution(&exec).await;
            if let Ok(Some(mut s)) = storage.get_script(&script_id).await {
                s.last_run = Some(chrono::Utc::now());
                s.run_count += 1;
                s.touch();
                let _ = storage.update_script(&s).await;
            }
        });

        Ok(exec_id)
    }

    pub async fn get_script_logs(
        &self,
        id_or_name: &str,
        execution_id: Option<&str>,
        lines: usize,
    ) -> anyhow::Result<Vec<String>> {
        // If id_or_name looks like an execution ID (or prefix), try resolving via
        // get_script_execution first so users can do: agenta script logs <exec_id>
        let (script_id, injected_exec_id) = if let Some(exec) = self.storage.get_script_execution(id_or_name).await? {
            (exec.script_id.clone(), Some(exec.id.clone()))
        } else {
            let script = self.storage.get_script(id_or_name).await?
                .ok_or_else(|| anyhow::anyhow!("Script not found: {}", id_or_name))?;
            (script.id.clone(), None)
        };

        // execution_id filter: prefer the one from the CLI arg, else from exec lookup
        let exec_filter = execution_id.map(|s| s.to_string()).or(injected_exec_id);

        let executions = self.storage.list_script_executions(&script_id, lines.max(50) as i64).await?;

        let mut log_lines = Vec::new();
        let filtered: Vec<_> = if let Some(ref exec_id) = exec_filter {
            executions.into_iter().filter(|e| e.id.starts_with(exec_id.as_str())).collect()
        } else {
            executions
        };

        for exec in filtered.iter().take(lines) {
            let exit_info = match exec.exit_code {
                Some(code) => format!(" (exit {})", code),
                None => String::new(),
            };
            log_lines.push(format!(
                "[{}] {} - {:?}{}",
                exec.started_at.format("%Y-%m-%d %H:%M:%S"),
                &exec.id[..8],
                exec.status,
                exit_info,
            ));
            if let Some(output) = &exec.output {
                if !output.trim().is_empty() {
                    for line in output.lines().take(50) {
                        log_lines.push(format!("  {}", line));
                    }
                }
            }
            if let Some(stderr) = &exec.stderr {
                if !stderr.trim().is_empty() {
                    log_lines.push("  --- stderr ---".to_string());
                    for line in stderr.lines().take(20) {
                        log_lines.push(format!("  {}", line));
                    }
                }
            }
            if let Some(err) = &exec.error {
                log_lines.push(format!("  ERROR: {}", err));
            }
        }

        Ok(log_lines)
    }
}

/// Expand a leading `~` to the actual home directory so that handlers stored as
/// `~/.agenta/scripts/foo.sh` work correctly when invoked without a shell.
fn expand_tilde(s: &str) -> String {
    if s.starts_with("~/") || s == "~" {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), &s[1..]);
        }
    }
    s.to_string()
}

async fn run_script_handler(handler: &str) -> anyhow::Result<(String, String, i32)> {
    let expanded = expand_tilde(handler);
    let mut parts = expanded.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("Empty handler for script"))?;
    let args: Vec<String> = parts.map(|a| expand_tilde(a)).collect();

    let output = Command::new(program)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn handler '{}': {}", handler, e))?
        .wait_with_output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);
    Ok((stdout, stderr, exit_code))
}
