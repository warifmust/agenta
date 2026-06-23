use chrono::Utc;
use regex::Regex;
use std::sync::Arc;
use tracing::info;

fn expand_home(path: &str) -> String {
    if path.starts_with("~/") || path == "~" {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), &path[1..]);
        }
    }
    path.to_string()
}

use crate::core::{
    Agent, DeepAgentConfig, ExecutionMode, ExecutionResult, Storage, ToolCall,
};
use crate::ollama::client::{ChatMessage, ChatRequest};
use crate::ollama::models::ModelParameters;
use crate::providers::ModelBackend;
use crate::scheduler::executor::AgentExecutor;
use crate::tools::{builtin_tool_descriptions, is_builtin_tool, run_tool};

/// Deep agent executor for multi-step reasoning
pub struct DeepAgentExecutor {
    backend: Arc<dyn ModelBackend>,
    storage: Arc<dyn Storage>,
    max_iterations: u32,
    stop_patterns: Vec<Regex>,
    /// Optional channel for emitting progress messages (e.g. Telegram notifications)
    progress_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl DeepAgentExecutor {
    pub fn new(base: &AgentExecutor, config: &DeepAgentConfig) -> anyhow::Result<Self> {
        let stop_patterns: Result<Vec<_>, _> = config
            .stop_conditions
            .iter()
            .map(|c| Regex::new(&format!(r"(?i){}", regex::escape(c))))
            .collect();

        Ok(Self {
            backend: base.backend(),
            storage: base.storage(),
            max_iterations: config.max_iterations,
            stop_patterns: stop_patterns?,
            progress_tx: base.progress_tx.clone(),
        })
    }

    /// Send a progress notification if a channel is attached.
    fn notify(&self, msg: impl Into<String>) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.send(msg.into());
        }
    }

    pub async fn execute_deep(
        &self,
        agent: &Agent,
        input: &str,
        execution: &mut ExecutionResult,
    ) -> anyhow::Result<String> {
        info!("Starting deep agent execution for agent: {}", agent.name);

        let mut conversation: Vec<ChatMessage> = vec![ChatMessage {
            role: "system".to_string(),
            content: format!(
                "{system}\n\nYou are a deep reasoning agent. Think step by step and reflect on your responses. You can iterate up to {max} times to complete a task. When finished, say 'TASK_COMPLETE:' followed by your final answer.",
                system = agent.system_prompt,
                max = self.max_iterations
            ),
        }];

        let params = ModelParameters::from_agent_config(&agent.config);

        conversation.push(ChatMessage {
            role: "user".to_string(),
            content: input.to_string(),
        });

        let mut final_response = String::new();

        for iteration in 0..self.max_iterations {
            execution.iterations = iteration + 1;
            info!("Deep agent iteration {}/{}", iteration + 1, self.max_iterations);

            let request = ChatRequest {
                model: agent.model.clone(),
                messages: conversation.clone(),
                stream: Some(false),
                format: None,
                options: Some(params.to_json_value()),
            };

            let response = self.backend.chat(request).await?;
            let assistant_response = response.message.content.clone();

            conversation.push(ChatMessage {
                role: "assistant".to_string(),
                content: assistant_response.clone(),
            });

            // Check for task completion
            if let Some(idx) = assistant_response.find("TASK_COMPLETE:") {
                final_response = assistant_response[idx + 14..].trim().to_string();
                info!("Deep agent completed task at iteration {}", iteration + 1);
                break;
            }

            // Check stop patterns
            if self.should_stop(&assistant_response) {
                final_response = assistant_response;
                info!("Deep agent stopped by stop condition at iteration {}", iteration + 1);
                break;
            }

            // If not complete, add reflection prompt
            if iteration < self.max_iterations - 1 {
                conversation.push(ChatMessage {
                    role: "user".to_string(),
                    content: "Reflect on your response. Is the task complete? If not, continue reasoning. If complete, say TASK_COMPLETE: followed by your final answer.".to_string(),
                });
            }

            final_response = assistant_response;
        }

        if final_response.is_empty() {
            final_response = "Task did not complete within iteration limit.".to_string();
        }

        Ok(final_response)
    }

    fn should_stop(&self, response: &str) -> bool {
        self.stop_patterns.iter().any(|p| p.is_match(response))
    }

    /// Build a memory block from the last 5 completed executions for this agent
    async fn build_memory_block(&self, agent: &Agent) -> String {
        let Ok(executions) = self.storage.list_executions(&agent.id, 10).await else {
            return String::new();
        };

        let past: Vec<String> = executions
            .into_iter()
            .filter(|e| e.output.is_some())
            .take(5)
            .map(|e| {
                let date = e.started_at.format("%Y-%m-%d").to_string();
                let output = e.output.unwrap_or_default();
                // Extract first line (usually the headline/title) as a summary
                let summary = output
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("(no output)")
                    .trim()
                    .trim_start_matches('#')
                    .trim()
                    .to_string();
                // Truncate to 120 chars
                let summary = if summary.len() > 120 {
                    format!("{}...", &summary[..117])
                } else {
                    summary
                };
                format!("[{}] {}", date, summary)
            })
            .collect();

        if past.is_empty() {
            return String::new();
        }

        format!(
            "MEMORY — YOUR PREVIOUS RUNS (most recent first):\n{}\n",
            past.join("\n")
        )
    }

    /// Execute deep agent with tool support
    pub async fn execute_deep_with_tools(
        &self,
        agent: &Agent,
        input: &str,
        execution: &mut ExecutionResult,
    ) -> anyhow::Result<String> {
        let params = ModelParameters::from_agent_config(&agent.config);

        // User-defined tools + built-in tools
        let mut tool_descriptions: Vec<String> = agent
            .tools
            .iter()
            .map(|t| format!("- {}: {}", t.name, t.description))
            .collect();
        for (name, desc) in builtin_tool_descriptions() {
            tool_descriptions.push(format!("- {} (built-in): {}", name, desc));
        }

        // Build memory block from past executions if memory is enabled
        let memory_block = if agent.memory_enabled {
            self.build_memory_block(agent).await
        } else {
            String::new()
        };

        let mut context = String::new();
        let mut iteration = 0;

        while iteration < self.max_iterations {
            execution.iterations = iteration + 1;

            let prompt = format!(
                "{system}\n\n{memory}You have access to these tools:\n{tools}\n\nCRITICAL INSTRUCTIONS:\n- Use tools step by step to complete this task.\n- To call a tool, your response MUST start with exactly: TOOL_CALL: {{\"tool\": \"tool_name\", \"parameters\": {{...}}}}\n- Example: TOOL_CALL: {{\"tool\": \"read_article\", \"parameters\": {{}}}}\n- After each tool result, decide: do you have more tools to call? If yes, call the next tool. If all steps are done, write TASK_COMPLETE: followed by your final summary.\n- Do NOT call the same tool twice.\n- Do NOT write TASK_COMPLETE until all required tool calls are finished.\n\nContext so far:\n{context}\n\nTask: {input}\n\nWhat is your next action? Call a tool or write TASK_COMPLETE if all steps are done:",
                system = agent.system_prompt,
                memory = memory_block,
                tools = tool_descriptions.join("\n"),
                context = if context.is_empty() {
                    "(No context yet)".to_string()
                } else {
                    context.clone()
                },
                input = input
            );

            let request = crate::ollama::client::GenerateRequest {
                model: agent.model.clone(),
                prompt,
                system: None,
                template: None,
                context: None,
                stream: Some(false),
                raw: None,
                format: None,
                options: Some(params.to_json_value()),
            };

            let response = self.backend.generate(request).await?;
            let content = response.response.clone();

            // Check for task completion
            if content.contains("TASK_COMPLETE:") {
                let final_idx = content.find("TASK_COMPLETE:").unwrap();
                return Ok(content[final_idx + 14..].trim().to_string());
            }

            // Check for tool call
            if content.contains("TOOL_CALL:") {
                if let Some(raw_result) = self.handle_tool_call(agent, &content).await {
                    // Truncate large tool outputs so they don't fill the context window.
                    const MAX_TOOL_OUTPUT: usize = 8_000;
                    let tool_result = if raw_result.len() > MAX_TOOL_OUTPUT {
                        format!("{}… [truncated, {} chars total]", &raw_result[..MAX_TOOL_OUTPUT], raw_result.len())
                    } else {
                        raw_result
                    };
                    context.push_str(&format!("\nAction: {}\nResult: {}", content, tool_result));

                    execution.tool_calls.push(ToolCall {
                        tool_name: "tool".to_string(),
                        parameters: serde_json::json!({"raw": content}),
                        result: tool_result.clone(),
                        timestamp: Utc::now(),
                    });
                } else {
                    context.push_str(&format!("\nAction: {}\nResult: Tool not found or failed", content));
                }
            } else {
                context.push_str(&format!("\nThought: {}", content));
            }

            iteration += 1;
        }

        Ok(format!(
            "Reached maximum iterations ({}). Final context:\n{}",
            self.max_iterations, context
        ))
    }

    async fn handle_tool_call(
        &self,
        agent: &Agent,
        content: &str,
    ) -> Option<String> {
        // Extract tool call from content
        if let Some(start) = content.find("TOOL_CALL:") {
            let json_start = content[start..].find('{')? + start;
            let mut depth = 0;
            let mut json_end = json_start;

            for (i, c) in content[json_start..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            json_end = json_start + i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }

            let json_str = &content[json_start..json_end];
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
                let tool_name = parsed.get("tool")?.as_str()?.to_string();
                let parameters = parsed.get("parameters")?.clone();

                // Check built-in tools first
                if is_builtin_tool(&tool_name) {
                    return self.run_builtin_tool(agent, &tool_name, &parameters).await;
                }

                match run_tool(agent, &tool_name, parameters).await {
                    Ok(output) => return Some(output),
                    Err(err) => return Some(format!("Tool error: {}", err)),
                }
            }
        }
        None
    }

    async fn run_builtin_tool(
        &self,
        parent: &Agent,
        tool_name: &str,
        parameters: &serde_json::Value,
    ) -> Option<String> {
        match tool_name {
            "read_file"   => self.builtin_read_file(parameters).await,
            "write_file"  => self.builtin_write_file(parameters).await,
            "list_files"  => self.builtin_list_files(parameters).await,
            "spawn_agent" => self.spawn_subagent(parent, parameters).await,
            _             => Some(format!("Unknown built-in tool: {}", tool_name)),
        }
    }

    async fn builtin_read_file(&self, params: &serde_json::Value) -> Option<String> {
        let raw = params.get("path")?.as_str()?;
        let path = expand_home(raw);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Some(content),
            Err(e) => Some(format!("Error reading {}: {}", path, e)),
        }
    }

    async fn builtin_write_file(&self, params: &serde_json::Value) -> Option<String> {
        let raw = params.get("path")?.as_str()?;
        let path = expand_home(raw);
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(&path, content).await {
            Ok(_) => Some(format!("Written {} bytes to {}", content.len(), path)),
            Err(e) => Some(format!("Error writing {}: {}", path, e)),
        }
    }

    async fn builtin_list_files(&self, params: &serde_json::Value) -> Option<String> {
        let raw = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let dir = expand_home(raw);
        let pattern = params.get("pattern").and_then(|v| v.as_str()).unwrap_or("*");
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) => return Some(format!("Error listing {}: {}", dir, e)),
        };
        let glob_pat = glob::Pattern::new(pattern).ok();
        let mut names: Vec<String> = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if glob_pat.as_ref().map_or(true, |p| p.matches(&name)) {
                names.push(name);
            }
        }
        names.sort();
        if names.is_empty() {
            Some(format!("No files matching '{}' in {}", pattern, dir))
        } else {
            Some(names.join("\n"))
        }
    }

    /// Spawn a sub-agent — either a named DB agent or an ephemeral one.
    async fn spawn_subagent(
        &self,
        parent: &Agent,
        parameters: &serde_json::Value,
    ) -> Option<String> {
        let input = parameters
            .get("input")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // If `name` is given, delegate to an existing DB agent.
        if let Some(agent_name) = parameters.get("name").and_then(|v| v.as_str()) {
            let task_preview = if input.len() > 80 {
                format!("{}…", &input[..80])
            } else {
                input.clone()
            };
            let spawn_msg = parent
                .deep_agent_config
                .as_ref()
                .and_then(|c| c.subagent_spawn_message.as_deref())
                .unwrap_or("⚙️ Delegating to {agent}: {task}")
                .replace("{agent}", agent_name)
                .replace("{task}", &task_preview);
            self.notify(spawn_msg);

            let named = match self.storage.get_agent_by_name(agent_name).await {
                Ok(Some(a)) => a,
                _ => return Some(format!("Agent not found: {}", agent_name)),
            };
            let executor = AgentExecutor::new(self.storage.clone(), self.backend.clone());
            return match executor.execute_ephemeral(&named, Some(input)).await {
                Ok(out) => Some(out),
                Err(e) => Some(format!("Agent {} failed: {}", agent_name, e)),
            };
        }

        // Fallback: ephemeral throwaway agent from `role`.
        let role = parameters.get("role")?.as_str()?.to_string();
        let model = parameters
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&parent.model)
            .to_string();

        // Build a human-readable task label from the input (first ~80 chars)
        let task_preview = {
            let trimmed = input.trim();
            if trimmed.len() > 80 {
                format!("{}…", &trimmed[..80])
            } else {
                trimmed.to_string()
            }
        };

        // Use custom spawn message if configured, otherwise fall back to generic default
        let spawn_msg = parent
            .deep_agent_config
            .as_ref()
            .and_then(|c| c.subagent_spawn_message.as_deref())
            .unwrap_or("⚙️ Spawning sub-agent: {task}")
            .replace("{task}", &task_preview);
        self.notify(spawn_msg);

        info!(
            "Spawning ephemeral sub-agent (model: {}) for parent: {}",
            model, parent.name
        );

        // Build ephemeral agent — never saved to DB
        let mut sub_agent = Agent::new(
            format!("sub-{}", uuid::Uuid::new_v4()),
            model,
            role,
        );
        sub_agent.execution_mode = ExecutionMode::Once;

        // Reuse existing executor — ephemeral, no DB writes
        let executor = AgentExecutor::new(self.storage.clone(), self.backend.clone());
        match executor.execute_ephemeral(&sub_agent, Some(input)).await {
            Ok(output) => {
                info!("Sub-agent completed for parent: {}", parent.name);
                Some(output)
            }
            Err(e) => Some(format!("Sub-agent failed: {}", e)),
        }
    }
}
