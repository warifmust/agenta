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

pub struct DeepAgentExecutor {
    backend: Arc<dyn ModelBackend>,
    storage: Arc<dyn Storage>,
    max_iterations: u32,
    stop_patterns: Vec<Regex>,
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

    fn notify(&self, msg: impl Into<String>) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.send(msg.into());
        }
    }

    fn should_stop(&self, response: &str) -> bool {
        self.stop_patterns.iter().any(|p| p.is_match(response))
    }

    /// Build a memory block from the last 5 completed executions for this agent.
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
                let summary = output
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("(no output)")
                    .trim()
                    .trim_start_matches('#')
                    .trim()
                    .to_string();
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
            "MEMORY — YOUR PREVIOUS RUNS (most recent first):\n{}\n\n",
            past.join("\n")
        )
    }

    /// ReAct loop using chat message history.
    /// Each tool result is fed back as a user message so the model sees a proper
    /// conversation rather than a growing flat prompt string.
    pub async fn execute_deep_with_tools(
        &self,
        agent: &Agent,
        input: &str,
        execution: &mut ExecutionResult,
    ) -> anyhow::Result<String> {
        let params = ModelParameters::from_agent_config(&agent.config);

        // Build tool catalogue for the system prompt
        let mut tool_descriptions: Vec<String> = agent
            .tools
            .iter()
            .map(|t| format!("- {}: {}", t.name, t.description))
            .collect();
        for (name, desc) in builtin_tool_descriptions() {
            tool_descriptions.push(format!("- {} (built-in): {}", name, desc));
        }

        let memory_block = if agent.memory_enabled {
            self.build_memory_block(agent).await
        } else {
            String::new()
        };

        let system_content = format!(
            "{system}\n\n{memory}You have access to these tools:\n{tools}\n\nINSTRUCTIONS:\n\
- To call a tool, respond with exactly: TOOL_CALL: {{\"tool\": \"tool_name\", \"parameters\": {{...}}}}\n\
- After receiving a TOOL_RESULT, decide: call another tool or write TASK_COMPLETE: <final answer>\n\
- Only write TASK_COMPLETE when all required steps are done.\n\
- You may iterate up to {max} times.",
            system  = agent.system_prompt,
            memory  = memory_block,
            tools   = tool_descriptions.join("\n"),
            max     = self.max_iterations,
        );

        let mut messages: Vec<ChatMessage> = vec![
            ChatMessage { role: "system".to_string(), content: system_content },
            ChatMessage { role: "user".to_string(),   content: input.to_string() },
        ];

        for iteration in 0..self.max_iterations {
            execution.iterations = iteration + 1;
            info!("Harness iteration {}/{} for agent: {}", iteration + 1, self.max_iterations, agent.name);

            let request = ChatRequest {
                model:    agent.model.clone(),
                messages: messages.clone(),
                stream:   Some(false),
                format:   None,
                options:  Some(params.to_json_value()),
            };

            let response = self.backend.chat(request).await?;
            let content  = response.message.content.clone();

            // Add assistant turn to history
            messages.push(ChatMessage {
                role:    "assistant".to_string(),
                content: content.clone(),
            });

            // ── Task complete ─────────────────────────────────────────────────
            // TASK_COMPLETE: is a "done" signal, not a position marker. Models vary
            // on whether they write the answer before or after it (e.g. gemma puts it
            // after; DeepSeek puts the full answer before and only a closer after).
            // Strip the marker and keep the whole message so we never drop the body.
            if content.contains("TASK_COMPLETE:") {
                info!("Agent {} completed at iteration {}", agent.name, iteration + 1);
                return Ok(content.replacen("TASK_COMPLETE:", "", 1).trim().to_string());
            }

            // ── Stop condition ────────────────────────────────────────────────
            if self.should_stop(&content) {
                info!("Agent {} stopped by condition at iteration {}", agent.name, iteration + 1);
                return Ok(content);
            }

            // ── Tool call ─────────────────────────────────────────────────────
            if content.contains("TOOL_CALL:") {
                let tool_result = match self.handle_tool_call(agent, &content).await {
                    Some(raw) => {
                        const MAX_TOOL_OUTPUT: usize = 8_000;
                        if raw.len() > MAX_TOOL_OUTPUT {
                            format!("{}… [truncated, {} chars total]", &raw[..MAX_TOOL_OUTPUT], raw.len())
                        } else {
                            raw
                        }
                    }
                    None => "Tool not found or failed.".to_string(),
                };

                execution.tool_calls.push(ToolCall {
                    tool_name:  "tool".to_string(),
                    parameters: serde_json::json!({"raw": content}),
                    result:     tool_result.clone(),
                    timestamp:  Utc::now(),
                });

                // Feed result back as the next user turn
                messages.push(ChatMessage {
                    role:    "user".to_string(),
                    content: format!("TOOL_RESULT: {}", tool_result),
                });

            } else {
                // No TOOL_CALL and no TASK_COMPLETE marker: the model produced a plain
                // prose response, which is its final answer. Return it directly instead
                // of nudging "Continue…" — that nudge makes the model emit a useless
                // "already answered" meta-comment on the next turn and discards the real
                // answer. Agents that use tools/TASK_COMPLETE are handled above; only
                // bare-prose answers (simple chat / RAG Q&A agents) reach here.
                info!("Agent {} answered in prose at iteration {}", agent.name, iteration + 1);
                return Ok(content.trim().to_string());
            }
        }

        // Max iterations hit — return the last assistant response
        let last = messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant")
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "Task did not complete within the iteration limit.".to_string());

        Ok(format!(
            "Reached maximum iterations ({}). Last response:\n{}",
            self.max_iterations, last
        ))
    }

    async fn handle_tool_call(&self, agent: &Agent, content: &str) -> Option<String> {
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
                let tool_name  = parsed.get("tool")?.as_str()?.to_string();
                let parameters = parsed.get("parameters")?.clone();

                if is_builtin_tool(&tool_name) {
                    return self.run_builtin_tool(agent, &tool_name, &parameters).await;
                }

                return match run_tool(agent, &tool_name, parameters).await {
                    Ok(output) => Some(output),
                    Err(err)   => Some(format!("Tool error: {}", err)),
                };
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
        let raw  = params.get("path")?.as_str()?;
        let path = expand_home(raw);
        match tokio::fs::read_to_string(&path).await {
            Ok(content) => Some(content),
            Err(e)      => Some(format!("Error reading {}: {}", path, e)),
        }
    }

    async fn builtin_write_file(&self, params: &serde_json::Value) -> Option<String> {
        let raw     = params.get("path")?.as_str()?;
        let path    = expand_home(raw);
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        match tokio::fs::write(&path, content).await {
            Ok(_)  => Some(format!("Written {} bytes to {}", content.len(), path)),
            Err(e) => Some(format!("Error writing {}: {}", path, e)),
        }
    }

    async fn builtin_list_files(&self, params: &serde_json::Value) -> Option<String> {
        let raw     = params.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let dir     = expand_home(raw);
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

        // Delegate to a named DB agent
        if let Some(agent_name) = parameters.get("name").and_then(|v| v.as_str()) {
            let task_preview = if input.len() > 80 { format!("{}…", &input[..80]) } else { input.clone() };
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
                _           => return Some(format!("Agent not found: {}", agent_name)),
            };
            let executor = AgentExecutor::new(self.storage.clone(), self.backend.clone());
            return match executor.execute_ephemeral(&named, Some(input)).await {
                Ok(out) => Some(out),
                Err(e)  => Some(format!("Agent {} failed: {}", agent_name, e)),
            };
        }

        // Ephemeral throwaway agent from `role`
        let role  = parameters.get("role")?.as_str()?.to_string();
        let model = parameters
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&parent.model)
            .to_string();

        let task_preview = {
            let t = input.trim();
            if t.len() > 80 { format!("{}…", &t[..80]) } else { t.to_string() }
        };

        let spawn_msg = parent
            .deep_agent_config
            .as_ref()
            .and_then(|c| c.subagent_spawn_message.as_deref())
            .unwrap_or("⚙️ Spawning sub-agent: {task}")
            .replace("{task}", &task_preview);
        self.notify(spawn_msg);

        info!("Spawning ephemeral sub-agent (model: {}) for parent: {}", model, parent.name);

        let mut sub_agent = Agent::new(format!("sub-{}", uuid::Uuid::new_v4()), model, role);
        sub_agent.execution_mode = ExecutionMode::Once;
        // Inherit the parent's provider so the sub-agent routes the same way (e.g.
        // an OpenRouter parent must not fall back to the default Ollama provider,
        // which can't serve the parent's model slugs).
        sub_agent.provider = parent.provider.clone();

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
