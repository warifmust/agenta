use chrono::Utc;
use regex::Regex;
use std::sync::Arc;
use tracing::info;

use crate::core::{
    Agent, DeepAgentConfig, ExecutionResult, Storage, ToolCall,
};
use crate::ollama::client::{ChatMessage, ChatRequest, OllamaClient};
use crate::ollama::models::ModelParameters;
use crate::scheduler::executor::AgentExecutor;
use crate::tools::run_tool;

/// Deep agent executor for multi-step reasoning
pub struct DeepAgentExecutor {
    ollama: OllamaClient,
    storage: Arc<dyn Storage>,
    max_iterations: u32,
    stop_patterns: Vec<Regex>,
}

impl DeepAgentExecutor {
    pub fn new(base: &AgentExecutor, config: &DeepAgentConfig) -> anyhow::Result<Self> {
        let stop_patterns: Result<Vec<_>, _> = config
            .stop_conditions
            .iter()
            .map(|c| Regex::new(&format!(r"(?i){}", regex::escape(c))))
            .collect();

        Ok(Self {
            ollama: base.ollama_client(),
            storage: base.storage(),
            max_iterations: config.max_iterations,
            stop_patterns: stop_patterns?,
        })
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

            let response = self.ollama.chat(request).await?;
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

        let tool_descriptions: Vec<String> = agent
            .tools
            .iter()
            .map(|t| format!("- {}: {}", t.name, t.description))
            .collect();

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

            let response = self.ollama.generate(request).await?;
            let content = response.response.clone();

            // Check for task completion
            if content.contains("TASK_COMPLETE:") {
                let final_idx = content.find("TASK_COMPLETE:").unwrap();
                return Ok(content[final_idx + 14..].trim().to_string());
            }

            // Check for tool call
            if content.contains("TOOL_CALL:") {
                if let Some(tool_result) = self.handle_tool_call(agent, &content).await {
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

                match run_tool(agent, &tool_name, parameters).await {
                    Ok(output) => return Some(output),
                    Err(err) => return Some(format!("Tool error: {}", err)),
                }
            }
        }
        None
    }
}
