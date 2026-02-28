use chrono::Utc;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

use crate::core::{
    Agent, AgentStatus, ExecutionMode, ExecutionResult, ExecutionStatus, Storage, ToolCall,
    ToolDefinition,
};
use crate::ollama::{
    client::{ChatMessage, ChatRequest, GenerateRequest, OllamaClient},
    models::ModelParameters,
};
use crate::tools::{run_tool, ToolInvocation};

#[derive(Clone)]
pub struct AgentExecutor {
    storage: Arc<dyn Storage>,
    ollama: OllamaClient,
}

impl AgentExecutor {
    pub fn new(storage: Arc<dyn Storage>, ollama: OllamaClient) -> Self {
        Self { storage, ollama }
    }

    pub fn ollama_client(&self) -> OllamaClient {
        self.ollama.clone()
    }

    pub async fn execute(&self, agent: &Agent, input: Option<String>) -> anyhow::Result<ExecutionResult> {
        let input_text = input.unwrap_or_else(|| "".to_string());
        let mut execution = ExecutionResult::new(agent.id.clone(), input_text.clone());
        self.execute_with_execution(agent, input_text, &mut execution).await?;
        Ok(execution)
    }

    pub async fn execute_with_id(
        &self,
        agent: &Agent,
        input: Option<String>,
        execution_id: String,
    ) -> anyhow::Result<ExecutionResult> {
        let input_text = input.unwrap_or_else(|| "".to_string());
        let mut execution =
            ExecutionResult::new_with_id(agent.id.clone(), input_text.clone(), execution_id);
        self.execute_with_execution(agent, input_text, &mut execution).await?;
        Ok(execution)
    }

    async fn execute_with_execution(
        &self,
        agent: &Agent,
        input_text: String,
        execution: &mut ExecutionResult,
    ) -> anyhow::Result<()> {
        execution.status = ExecutionStatus::Running;
        self.storage.create_execution(&execution).await?;

        // Update agent status
        let mut updated_agent = agent.clone();
        updated_agent.status = AgentStatus::Running;
        updated_agent.last_run = Some(Utc::now());
        updated_agent.run_count += 1;
        self.storage.update_agent(&updated_agent).await?;

        // Bound execution time so runs do not stay "running" forever.
        let execution_timeout = Duration::from_secs(180);
        let result = tokio::time::timeout(
            execution_timeout,
            self.run_execution(agent, &input_text, execution),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Execution timed out after {} seconds", execution_timeout.as_secs()))?;

        // Update execution result
        execution.completed_at = Some(Utc::now());
        match result {
            Ok(output) => {
                execution.output = Some(output);
                execution.status = ExecutionStatus::Completed;
            }
            Err(e) => {
                execution.error = Some(e.to_string());
                execution.status = ExecutionStatus::Failed;
            }
        }

        self.storage.update_execution(&execution).await?;

        // Update agent status back to active
        updated_agent.status = AgentStatus::Active;
        self.storage.update_agent(&updated_agent).await?;

        Ok(())
    }

    async fn run_execution(
        &self,
        agent: &Agent,
        input_text: &str,
        execution: &mut ExecutionResult,
    ) -> anyhow::Result<String> {
        if let Some(config) = &agent.deep_agent_config {
            let deep = crate::scheduler::DeepAgentExecutor::new(self, config)?;
            if agent.tools.is_empty() {
                deep.execute_deep(agent, input_text, execution).await
            } else {
                deep.execute_deep_with_tools(agent, input_text, execution).await
            }
        } else {
            match agent.execution_mode {
                ExecutionMode::Once | ExecutionMode::Scheduled | ExecutionMode::Triggered => {
                    if agent.tools.is_empty() {
                        self.execute_single(agent, input_text, execution).await
                    } else {
                        self.execute_with_tools(agent, input_text, execution).await
                    }
                }
                ExecutionMode::Continuous => {
                    if agent.tools.is_empty() {
                        self.execute_single(agent, input_text, execution).await
                    } else {
                        self.execute_with_tools(agent, input_text, execution).await
                    }
                }
            }
        }
    }

    async fn execute_single(
        &self,
        agent: &Agent,
        input: &str,
        execution: &mut ExecutionResult,
    ) -> anyhow::Result<String> {
        let params = ModelParameters::from_agent_config(&agent.config);

        // Simple generation without chat history
        if input.is_empty() {
            // Generate based on system prompt only
            let request = GenerateRequest {
                model: agent.model.clone(),
                prompt: agent.system_prompt.clone(),
                system: None,
                template: None,
                context: None,
                stream: Some(false),
                raw: None,
                format: None,
                options: Some(params.to_json_value()),
            };

            let response = self.ollama.generate(request).await?;
            Ok(response.response)
        } else {
            // Chat-based interaction with automatic memory from recent executions.
            let messages = self
                .build_chat_messages_with_memory(agent, input, &execution.id)
                .await;

            let request = ChatRequest {
                model: agent.model.clone(),
                messages,
                stream: Some(false),
                format: None,
                options: Some(params.to_json_value()),
            };

            let response = self.ollama.chat(request).await?;
            Ok(response.message.content)
        }
    }

    async fn build_chat_messages_with_memory(
        &self,
        agent: &Agent,
        input: &str,
        current_execution_id: &str,
    ) -> Vec<ChatMessage> {
        let mut messages = vec![ChatMessage {
            role: "system".to_string(),
            content: agent.system_prompt.clone(),
        }];

        // Load recent completed turns for this agent.
        // We keep a small memory window to avoid huge prompts.
        //
        // Important: only feed prior user inputs back into memory, not prior
        // assistant outputs. This avoids style lock-in (e.g., old markdown/table
        // formatting) after system prompt changes.
        let history = self.storage.list_executions(&agent.id, 20).await;
        if let Ok(executions) = history {
            let mut prior_user_inputs: Vec<_> = executions
                .into_iter()
                .filter(|e| e.id != current_execution_id)
                .filter(|e| !e.input.trim().is_empty())
                .map(|e| e.input)
                .collect();

            // list_executions is newest-first; reverse to chronological order.
            prior_user_inputs.reverse();
            // Keep last 6 user turns.
            let start = prior_user_inputs.len().saturating_sub(6);
            for user_input in prior_user_inputs.into_iter().skip(start) {
                messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: user_input,
                });
            }
        }

        messages.push(ChatMessage {
            role: "user".to_string(),
            content: input.to_string(),
        });
        messages
    }

    pub async fn execute_with_tools(
        &self,
        agent: &Agent,
        input: &str,
        execution: &mut ExecutionResult,
    ) -> anyhow::Result<String> {
        let available_tools = self.resolve_available_tools(agent).await?;
        if available_tools.is_empty() {
            return self.execute_single(agent, input, execution).await;
        }

        // Build prompt with tool descriptions
        let tool_descriptions: Vec<String> = available_tools
            .iter()
            .map(|t| format!("- {}: {}", t.name, t.description))
            .collect();

        let enhanced_prompt = format!(
            "{system_prompt}\n\nYou have access to the following tools:\n{tools}\n\nIf you need to use a tool, respond with: TOOL_CALL: {{\"tool\": \"tool_name\", \"parameters\": {{...}}}}\n\nUser: {input}\n\nAssistant:",
            system_prompt = agent.system_prompt,
            tools = tool_descriptions.join("\n"),
            input = input
        );

        let params = ModelParameters::from_agent_config(&agent.config);
        let request = GenerateRequest {
            model: agent.model.clone(),
            prompt: enhanced_prompt.clone(),
            system: None,
            template: None,
            context: None,
            stream: Some(false),
            raw: None,
            format: None,
            options: Some(params.to_json_value()),
        };

        let response = self.ollama.generate(request).await?;
        let content = response.response;

        // Check for tool calls in response
        if content.contains("TOOL_CALL:") {
            if let Some(tool_call) = self.parse_tool_call(&content) {
                match self.execute_tool(&agent, &available_tools, &tool_call).await {
                    Ok(result) => {
                        execution.tool_calls.push(ToolCall {
                            tool_name: tool_call.name.clone(),
                            parameters: tool_call.parameters.clone(),
                            result: result.clone(),
                            timestamp: Utc::now(),
                        });

                        // Continue with tool result
                        let follow_up = format!(
                            "{enhanced_prompt}\n{content}\n\nTool result: {result}\n\nFinal response:",
                            enhanced_prompt = enhanced_prompt,
                            content = content,
                            result = result
                        );

                        let follow_request = GenerateRequest {
                            model: agent.model.clone(),
                            prompt: follow_up,
                            system: None,
                            template: None,
                            context: None,
                            stream: Some(false),
                            raw: None,
                            format: None,
                            options: Some(params.to_json_value()),
                        };

                        let follow_response = self.ollama.generate(follow_request).await?;
                        return Ok(follow_response.response);
                    }
                    Err(e) => {
                        warn!("Tool execution failed: {}", e);
                        return Ok(content);
                    }
                }
            }
        }

        Ok(content)
    }

    fn parse_tool_call(&self, content: &str) -> Option<ToolInvocation> {
        // Simple parsing - extract TOOL_CALL: {...} from response
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
                let name = parsed.get("tool")?.as_str()?.to_string();
                return Some(ToolInvocation {
                    name,
                    parameters: parsed.get("parameters")?.clone(),
                });
            }

            // Fallback: some models return JSON-like payloads with raw newlines
            // inside string fields, which is invalid JSON. Escape those and retry.
            let repaired = escape_newlines_in_json_strings(json_str);
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&repaired) {
                let name = parsed.get("tool")?.as_str()?.to_string();
                return Some(ToolInvocation {
                    name,
                    parameters: parsed.get("parameters")?.clone(),
                });
            }

            // Last-resort fallback for malformed document_render payloads.
            if let Some(invocation) = parse_document_render_fallback(json_str) {
                return Some(invocation);
            }
        }
        None
    }

    async fn execute_tool(
        &self,
        agent: &Agent,
        available_tools: &[ToolDefinition],
        tool: &ToolInvocation,
    ) -> anyhow::Result<String> {
        let mut runtime_agent = agent.clone();
        runtime_agent.tools = available_tools.to_vec();
        run_tool(&runtime_agent, &tool.name, tool.parameters.clone()).await
    }

    async fn resolve_available_tools(&self, agent: &Agent) -> anyhow::Result<Vec<ToolDefinition>> {
        let mut merged = agent.tools.clone();
        let mut seen: HashSet<String> = merged.iter().map(|t| t.name.clone()).collect();

        let registry_tools = self.storage.list_tools().await?;
        for tool in registry_tools {
            if !tool.enabled || tool.handler.is_none() || seen.contains(&tool.name) {
                continue;
            }
            seen.insert(tool.name.clone());
            merged.push(tool.as_definition());
        }

        Ok(merged)
    }
}

fn escape_newlines_in_json_strings(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    let mut in_string = false;
    let mut escaped = false;

    for ch in input.chars() {
        if in_string {
            match ch {
                '\\' if !escaped => {
                    escaped = true;
                    out.push(ch);
                    continue;
                }
                '"' if !escaped => {
                    in_string = false;
                    out.push(ch);
                }
                '\n' => out.push_str("\\n"),
                '\r' => {}
                _ => out.push(ch),
            }
            escaped = false;
            continue;
        }

        if ch == '"' {
            in_string = true;
        }
        out.push(ch);
    }

    out
}

fn parse_document_render_fallback(input: &str) -> Option<ToolInvocation> {
    if !input.contains("\"tool\"") || !input.contains("document_render") {
        return None;
    }

    fn find_quoted_value(haystack: &str, key: &str) -> Option<String> {
        let marker = format!("\"{}\":\"", key);
        let start = haystack.find(&marker)? + marker.len();
        let rest = &haystack[start..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    let format = find_quoted_value(input, "format").unwrap_or_else(|| "pdf".to_string());
    let title = find_quoted_value(input, "title").unwrap_or_else(|| "Trip Itinerary".to_string());
    let filename = find_quoted_value(input, "filename").unwrap_or_else(|| "itinerary.pdf".to_string());

    let content = if let Some(marker_pos) = input.find("\"content\":\"") {
        let start = marker_pos + "\"content\":\"".len();
        let tail = &input[start..];
        // Prefer ending at `"} }` style suffixes if present.
        let mut end = tail.len();
        for pat in ["\"}}", "\"}\n", "\"}"] {
            if let Some(i) = tail.rfind(pat) {
                end = i;
                break;
            }
        }
        let raw = &tail[..end];
        raw.replace("\\n", "\n")
            .replace("\\r", "")
            .replace("\\t", "\t")
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        return None;
    };

    Some(ToolInvocation {
        name: "document_render".to_string(),
        parameters: serde_json::json!({
            "format": format,
            "title": title,
            "filename": filename,
            "content": content
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::{escape_newlines_in_json_strings, parse_document_render_fallback};

    #[test]
    fn escapes_newlines_inside_json_string_values() {
        let raw = "{\"tool\":\"document_render\",\"parameters\":{\"content\":\"line1\nline2\"}}";
        let repaired = escape_newlines_in_json_strings(raw);
        let parsed: serde_json::Value = serde_json::from_str(&repaired).expect("json should parse");
        assert_eq!(parsed["parameters"]["content"], "line1\nline2");
    }

    #[test]
    fn fallback_parses_malformed_document_render_payload() {
        let malformed = "{\"tool\":\"document_render\",\"parameters\":{\"format\":\"pdf\",\"title\":\"Bangkok itinerary\",\"filename\":\"itinerary.pdf\",\"content\":\"Trip Snapshot\nDestination: Bangkok\nDay 1: Grand Palace\"}}";
        let invocation = parse_document_render_fallback(malformed).expect("should parse fallback");
        assert_eq!(invocation.name, "document_render");
        assert_eq!(invocation.parameters["format"], "pdf");
        assert_eq!(invocation.parameters["filename"], "itinerary.pdf");
        assert!(invocation.parameters["content"]
            .as_str()
            .unwrap_or_default()
            .contains("Destination: Bangkok"));
    }
}
