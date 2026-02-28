use std::collections::HashMap;
use std::sync::Arc;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use agenta::core::{
    Agent, AgentStatus, ExecutionMode, ExecutionResult, Storage, ToolExecution,
    ToolExecutionStatus, ToolResource, TriggerEvent, TriggerType,
};
use agenta::ollama::OllamaClient;
use agenta::scheduler::AgentExecutor;
use agenta::trigger::{CommandTrigger, FileWatcherTrigger, HttpTrigger, Scheduler as CronScheduler};

pub struct DaemonState {
    storage: Arc<dyn Storage>,
    ollama: OllamaClient,
    executor: AgentExecutor,
    cron_scheduler: CronScheduler,
    running_agents: Arc<RwLock<HashMap<String, tokio::task::JoinHandle<()>>>>,
    http_trigger: Arc<RwLock<Option<HttpTrigger>>>,
    file_watcher: Arc<RwLock<Option<FileWatcherTrigger>>>,
    command_triggers: Arc<RwLock<HashMap<String, CommandTrigger>>>,
    event_sender: Arc<RwLock<Option<tokio::sync::mpsc::Sender<TriggerEvent>>>>,
}

impl DaemonState {
    pub async fn new(storage: Arc<dyn Storage>, ollama_url: String) -> anyhow::Result<Self> {
        let ollama = OllamaClient::new(ollama_url);
        let executor = AgentExecutor::new(storage.clone(), ollama.clone());
        let cron_scheduler = CronScheduler::new();

        Ok(Self {
            storage,
            ollama,
            executor,
            cron_scheduler,
            running_agents: Arc::new(RwLock::new(HashMap::new())),
            http_trigger: Arc::new(RwLock::new(None)),
            file_watcher: Arc::new(RwLock::new(None)),
            command_triggers: Arc::new(RwLock::new(HashMap::new())),
            event_sender: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn start_background_tasks(&self) -> anyhow::Result<()> {
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

        // Handle trigger events
        let storage = self.storage.clone();
        let executor = self.executor.clone();

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                match event {
                    TriggerEvent::Scheduled { agent_id, cron } => {
                        info!("Scheduled trigger for {}: {}", agent_id, cron);
                        if let Ok(Some(agent)) = storage.get_agent(&agent_id).await {
                            let _ = executor.execute(&agent, None).await;
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
                                        let _ = executor.execute(&agent, Some(path.clone())).await;
                                    }
                                }
                            }
                        }
                    }
                    TriggerEvent::HttpRequest { agent_id, method, path, body } => {
                        info!("HTTP trigger: {} {} -> {}", method, path, agent_id);
                        if let Ok(Some(agent)) = storage.get_agent(&agent_id).await {
                            let input = body.unwrap_or_default();
                            let _ = executor.execute(&agent, Some(input)).await;
                        }
                    }
                    TriggerEvent::CommandOutput { agent_id, command, output, matched: _ } => {
                        info!("Command trigger: {} -> {}", command, agent_id);
                        if let Ok(Some(agent)) = storage.get_agent(&agent_id).await {
                            let _ = executor.execute(&agent, Some(output)).await;
                        }
                    }
                    _ => {}
                }
            }
        });

        Ok(())
    }

    pub async fn create_agent(&self,
        mut agent: Agent,
    ) -> anyhow::Result<String> {
        agent.status = AgentStatus::Active;
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
        if let Some(agent) = self.storage.get_agent(id).await? {
            return Ok(Some(agent));
        }
        Ok(self.storage.get_agent_by_name(id).await?)
    }

    pub async fn list_agents(&self,
    ) -> anyhow::Result<Vec<Agent>> {
        Ok(self.storage.list_agents().await?)
    }

    pub async fn run_agent(
        &self,
        id: &str,
        input: Option<String>,
    ) -> anyhow::Result<String> {
        let agent = self
            .get_agent(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found"))?;

        let storage = self.storage.clone();
        let executor = self.executor.clone();

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

    pub async fn run_agent_sync_execution(
        &self,
        id: &str,
        input: String,
    ) -> anyhow::Result<ExecutionResult> {
        let agent = self
            .get_agent(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found"))?;

        let execution_id = uuid::Uuid::new_v4().to_string();
        self
            .executor
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
            if let Some(execution) = self.storage.get_execution(exec_id).await? {
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

    pub async fn update_tool(&self, id_or_name: &str, mut tool: ToolResource) -> anyhow::Result<()> {
        let existing = self
            .get_tool(id_or_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Tool not found"))?;
        tool.id = existing.id;
        tool.created_at = existing.created_at;
        tool.updated_at = chrono::Utc::now();
        self.storage.update_tool(&tool).await?;
        Ok(())
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
}

async fn run_tool_handler(
    tool: &ToolResource,
    parameters: serde_json::Value,
) -> anyhow::Result<String> {
    let handler = tool
        .handler
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Tool has no handler"))?;

    let mut parts = handler.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("Invalid tool handler: {}", handler))?;
    let args: Vec<&str> = parts.collect();

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("AGENTA_TOOL_NAME", &tool.name)
        .env("AGENTA_TOOL_PARAMS", parameters.to_string())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(parameters.to_string().as_bytes()).await?;
    }

    let output = child.wait_with_output().await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(anyhow::anyhow!(
            "Tool {} failed ({}): {}",
            tool.name,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
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
}
