use anyhow::anyhow;
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
    Agent, AgentStatus, DeepAgentConfig, ExecutionMode, ExecutionResult, Proposal, ProposalAction,
    SideEffect, Storage, ToolCall, ToolResource,
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
        let mind = crate::core::is_mind(agent);
        for (name, desc) in builtin_tool_descriptions() {
            // MIND is read-and-propose only: it must never mutate the filesystem
            // directly, so it doesn't get write_file. All its changes go through
            // propose_* (human-approved). Other agents keep write_file.
            if mind && name == "write_file" {
                continue;
            }
            tool_descriptions.push(format!("- {} (built-in): {}", name, desc));
        }

        let memory_block = if agent.memory_enabled {
            self.build_memory_block(agent).await
        } else {
            String::new()
        };

        let system_content = format!(
            "{system}\n\n{memory}You have access to these tools:\n{tools}\n\nINSTRUCTIONS:\n\
- To call a tool, respond with EXACTLY one line and nothing else: TOOL_CALL: {{\"tool\": \"tool_name\", \"parameters\": {{...}}}}\n\
- The SYSTEM runs the tool automatically and sends you a TOOL_RESULT on your next turn. NEVER ask the user to run a tool or to provide/paste results — they are delivered to you automatically.\n\
- Emit only ONE TOOL_CALL per turn, and put no other text in that turn. Then stop and wait for the TOOL_RESULT before doing anything else.\n\
- After you have the TOOL_RESULT(s) you need, write TASK_COMPLETE: <final answer> using them.\n\
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

        // Sum tokens across every model call in this run, stashed into the
        // execution's metadata after each call so it's present no matter how the
        // loop exits. Metadata is a free-form JSON blob, so no schema change.
        let mut total_tokens: u64 = 0;
        // Peak input/context tokens across iterations — how full the context got at
        // its fullest (the last iteration usually has the biggest prompt).
        let mut peak_context: u64 = 0;

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

            total_tokens += response.tokens().unwrap_or(0);
            peak_context = peak_context.max(response.context_tokens().unwrap_or(0));
            if total_tokens > 0 || peak_context > 0 {
                if !execution.metadata.is_object() {
                    execution.metadata = serde_json::json!({});
                }
                execution.metadata["total_tokens"] = serde_json::json!(total_tokens);
                execution.metadata["peak_context_tokens"] = serde_json::json!(peak_context);
            }

            // ── Unusable turn ─────────────────────────────────────────────────
            // Reasoning models (DeepSeek et al.) spend max_tokens on their reasoning
            // BEFORE writing any answer, so too small a budget yields `content: null`
            // (reasoning ate all of it) or an answer cut off mid-sentence. Neither is
            // a usable turn, and both used to sail through: an empty answer was
            // returned as "prose" and looked like success, while a truncated one — a
            // half-written system prompt, a chopped TOOL_CALL — read as complete.
            // Fail loudly, with the numbers needed to fix it.
            if content.trim().is_empty() || response.truncated() {
                let budget = params.num_predict.unwrap_or(0);
                let reason = if content.trim().is_empty() {
                    "returned no content (its reasoning used the whole output budget)"
                } else {
                    "was cut off mid-answer"
                };
                // Report OUTPUT tokens, not total: the budget caps output only, so
                // quoting prompt+completion here would point at the wrong number.
                let output_used = match (response.tokens(), response.context_tokens()) {
                    (Some(total), Some(prompt)) => total.saturating_sub(prompt),
                    (Some(total), None) => total,
                    _ => 0,
                };
                return Err(anyhow!(
                    "Model {} {} at iteration {}. It stopped because: {}. \
                     It generated {} output tokens against a max_tokens of {}. \
                     Raise it (e.g. `agenta update {} --max-tokens {}`).",
                    agent.model,
                    reason,
                    iteration + 1,
                    response.finish_reason.as_deref().unwrap_or("unknown"),
                    output_used,
                    budget,
                    agent.name,
                    (budget * 2).max(16_000),
                ));
            }

            // Add assistant turn to history
            messages.push(ChatMessage {
                role:    "assistant".to_string(),
                content: content.clone(),
            });

            // ── Tool call FIRST ───────────────────────────────────────────────
            // Execute any TOOL_CALL before checking completion/stop. Models often
            // emit a TOOL_CALL *and* a TASK_COMPLETE (or other stop-word) in the
            // same message; if we checked completion first, the tool would never
            // run. Run it, feed the result back, and let the model finish next turn.
            if content.contains("TOOL_CALL:") {
                let calls = self.handle_tool_call(agent, &content).await;

                // Record each call with its real tool name (so callers/UIs can
                // show "called get_agents" traces), and build the fed-back result.
                let tool_result = if calls.is_empty() {
                    "Tool not found or failed.".to_string()
                } else {
                    for (name, result) in &calls {
                        execution.tool_calls.push(ToolCall {
                            tool_name:  name.clone(),
                            parameters: serde_json::json!({}),
                            result:     result.chars().take(4_000).collect::<String>(),
                            timestamp:  Utc::now(),
                        });
                    }
                    let joined = calls
                        .iter()
                        .map(|(n, r)| format!("{}: {}", n, r))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    const MAX_TOOL_OUTPUT: usize = 8_000;
                    if joined.len() > MAX_TOOL_OUTPUT {
                        format!("{}… [truncated, {} chars total]", &joined[..MAX_TOOL_OUTPUT], joined.len())
                    } else {
                        joined
                    }
                };

                // Feed result back as the next user turn, then continue the loop.
                messages.push(ChatMessage {
                    role:    "user".to_string(),
                    content: format!("TOOL_RESULT: {}", tool_result),
                });
                continue;
            }

            // ── Task complete ─────────────────────────────────────────────────
            // TASK_COMPLETE: is a "done" signal (no tool call in this message).
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

            // No TOOL_CALL and no completion marker: the model produced a plain
            // prose response, which is its final answer.
            info!("Agent {} answered in prose at iteration {}", agent.name, iteration + 1);
            return Ok(content.trim().to_string());
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

    /// Execute EVERY `TOOL_CALL:` in the message, not just the first — models
    /// (e.g. gpt-5.4) often batch several calls in one turn. Returns
    /// `(tool_name, result)` pairs in call order so the loop can record and feed
    /// each back.
    async fn handle_tool_call(&self, agent: &Agent, content: &str) -> Vec<(String, String)> {
        let mut results: Vec<(String, String)> = Vec::new();
        let mut cursor = 0usize;

        while let Some(rel) = content[cursor..].find("TOOL_CALL:") {
            let marker = cursor + rel;
            // Find the JSON object after this marker (brace-matched).
            let json_start = match content[marker..].find('{') {
                Some(p) => marker + p,
                None => break,
            };
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
            // Advance past this call regardless of parse outcome.
            cursor = json_end.max(marker + "TOOL_CALL:".len());

            let json_str = &content[json_start..json_end];
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) else {
                continue;
            };
            let Some(tool_name) = parsed.get("tool").and_then(|v| v.as_str()) else {
                continue;
            };
            // Parameters default to an empty object (many read tools take none).
            let parameters = parsed
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            let output = if is_builtin_tool(tool_name) {
                self.run_builtin_tool(agent, tool_name, &parameters)
                    .await
                    .unwrap_or_else(|| "Tool produced no output.".to_string())
            } else {
                match run_tool(agent, tool_name, parameters).await {
                    Ok(out) => out,
                    Err(err) => format!("Tool error: {}", err),
                }
            };

            results.push((tool_name.to_string(), output));
        }

        results
    }

    async fn run_builtin_tool(
        &self,
        parent: &Agent,
        tool_name: &str,
        parameters: &serde_json::Value,
    ) -> Option<String> {
        match tool_name {
            "read_file"   => self.builtin_read_file(parameters).await,
            // MIND never writes files directly — it is read-and-propose only. Refuse
            // here too (defense in depth beyond hiding it from MIND's tool list).
            "write_file" if crate::core::is_mind(parent) => Some(
                "Refused: MIND is read-and-propose only and cannot write files directly. \
                 To create a tool, script, or file-producing capability, use propose_create_tool \
                 so the user approves it before anything is written."
                    .to_string(),
            ),
            "write_file"  => self.builtin_write_file(parameters).await,
            "list_files"  => self.builtin_list_files(parameters).await,
            "spawn_agent" => self.spawn_subagent(parent, parameters).await,
            "list_tools"  => self.builtin_list_tools().await,
            "list_agents" => self.builtin_list_agents().await,
            "get_tool"    => self.builtin_get_tool(parameters).await,
            "get_agent"   => self.builtin_get_agent(parameters).await,
            "propose_create_tool" => self.builtin_propose_create_tool(parent, parameters).await,
            "propose_create_agent" => self.builtin_propose_create_agent(parent, parameters).await,
            "propose_update_agent" => self.builtin_propose_update_agent(parent, parameters).await,
            "propose_update_tool" => self.builtin_propose_update_tool(parent, parameters).await,
            "propose_attach_kb" => self.builtin_propose_kb(parent, parameters, true).await,
            "propose_detach_kb" => self.builtin_propose_kb(parent, parameters, false).await,
            "check_command" => self.builtin_check_command(parameters).await,
            "remember_feedback" => self.builtin_remember_feedback(parent, parameters).await,
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

    // ── Read tier: observe the ecosystem (no side effects, no approval) ─────

    async fn builtin_list_tools(&self) -> Option<String> {
        match self.storage.list_tools().await {
            Ok(tools) if !tools.is_empty() => {
                let mut out = format!("{} tool(s):\n", tools.len());
                for t in &tools {
                    let kind = if t.http.is_some() { "http" } else { "script" };
                    let desc: String = t.description.chars().take(80).collect();
                    out.push_str(&format!(
                        "- {} [{}, {:?}]: {}\n",
                        t.name, kind, t.side_effect, desc
                    ));
                }
                Some(out)
            }
            Ok(_) => Some("No tools exist yet.".to_string()),
            Err(e) => Some(format!("Error listing tools: {}", e)),
        }
    }

    async fn builtin_list_agents(&self) -> Option<String> {
        match self.storage.list_agents().await {
            Ok(agents) if !agents.is_empty() => {
                let mut out = format!("{} agent(s):\n", agents.len());
                for a in &agents {
                    let kb = if a.config.knowledge_bases.is_empty() {
                        String::new()
                    } else {
                        format!(", KB: {}", a.config.knowledge_bases.join("+"))
                    };
                    out.push_str(&format!(
                        "- {} [{}, {:?}]{}\n",
                        a.name, a.model, a.status, kb
                    ));
                }
                Some(out)
            }
            Ok(_) => Some("No agents exist yet.".to_string()),
            Err(e) => Some(format!("Error listing agents: {}", e)),
        }
    }

    async fn builtin_get_tool(&self, params: &serde_json::Value) -> Option<String> {
        let name = params.get("name").and_then(|v| v.as_str())?;
        match self.storage.get_tool_by_name(name).await {
            Ok(Some(t)) => {
                let kind = if t.http.is_some() { "http" } else { "script" };
                Some(format!(
                    "Tool {}\n  description: {}\n  type: {}\n  side_effect: {:?}\n  secrets: {:?}\n  handler: {}\n  parameters: {}",
                    t.name,
                    t.description,
                    kind,
                    t.side_effect,
                    t.secrets,
                    t.handler.as_deref().unwrap_or("N/A"),
                    t.parameters
                ))
            }
            Ok(None) => Some(format!("No tool named '{}'.", name)),
            Err(e) => Some(format!("Error getting tool: {}", e)),
        }
    }

    async fn builtin_get_agent(&self, params: &serde_json::Value) -> Option<String> {
        let name = params.get("name").and_then(|v| v.as_str())?;
        match self.storage.get_agent_by_name(name).await {
            Ok(Some(a)) => Some(format!(
                "Agent {}\n  model: {} ({})\n  status: {:?}\n  mode: {:?}\n  knowledge_bases: {:?}\n  tools: {:?}\n  system_prompt: {}",
                a.name,
                a.model,
                a.provider.as_deref().unwrap_or("ollama"),
                a.status,
                a.execution_mode,
                a.config.knowledge_bases,
                a.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
                a.system_prompt.chars().take(200).collect::<String>()
            )),
            Ok(None) => Some(format!("No agent named '{}'.", name)),
            Err(e) => Some(format!("Error getting agent: {}", e)),
        }
    }

    /// MIND's write-tier builtin: does NOT create the tool. It drafts a Proposal
    /// the user must approve. Returns a result that tells MIND to report and wait.
    async fn builtin_propose_create_tool(
        &self,
        parent: &Agent,
        params: &serde_json::Value,
    ) -> Option<String> {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => return Some("Error: propose_create_tool requires a non-empty 'name'.".to_string()),
        };
        let description = params
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let parameters = params
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({ "type": "object" }));
        let handler = params.get("handler").and_then(|v| v.as_str()).map(|s| s.to_string());

        let mut tool = ToolResource::new(name, description, parameters, handler);
        if let Some(secrets) = params.get("secrets").and_then(|v| v.as_array()) {
            tool.secrets = secrets.iter().filter_map(|s| s.as_str().map(String::from)).collect();
        }
        if let Some(se) = params.get("side_effect").and_then(|v| v.as_str()) {
            tool.side_effect = match se.to_lowercase().replace('-', "_").as_str() {
                "write" => SideEffect::Write,
                "destructive" => SideEffect::Destructive,
                _ => SideEffect::ReadOnly,
            };
        }
        if let Some(http) = params.get("http").filter(|h| !h.is_null()) {
            tool.http = serde_json::from_value(http.clone()).ok();
        }

        let rationale = params
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let proposal = Proposal::new(ProposalAction::CreateTool(tool), rationale, parent.name.clone());
        let short: String = proposal.id.chars().take(8).collect();
        match self.storage.create_proposal(&proposal).await {
            Ok(_) => Some(format!(
                "Proposal {} created: {} (risk: {:?}). IMPORTANT: this is NOT applied — the user must approve it. \
                 Do not claim the tool exists. Tell the user what you proposed and that it awaits their approval \
                 (they can run `agenta approve {}` or review with `agenta proposals`).",
                short,
                proposal.summary(),
                proposal.risk,
                short,
            )),
            Err(e) => Some(format!("Error creating proposal: {}", e)),
        }
    }

    /// MIND's write-tier builtin: proposes creating a new agent (never applies).
    async fn builtin_propose_create_agent(
        &self,
        parent: &Agent,
        params: &serde_json::Value,
    ) -> Option<String> {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => return Some("Error: propose_create_agent requires a non-empty 'name'.".to_string()),
        };
        // Default model/provider to MIND's own (known-configured) when unspecified.
        let model = params
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| parent.model.clone());
        let system_prompt = params
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("You are a helpful agenta agent.")
            .to_string();

        let mut agent = Agent::new(name, model, system_prompt);
        agent.status = AgentStatus::Active;
        agent.provider = params
            .get("provider")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| parent.provider.clone());
        if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
            agent.description = Some(desc.to_string());
        }

        // Attach requested tools by name, resolved from the DB registry.
        let mut missing: Vec<String> = Vec::new();
        if let Some(tools) = params.get("tools").and_then(|v| v.as_array()) {
            for t in tools {
                if let Some(tn) = t.as_str() {
                    match self.storage.get_tool_by_name(tn).await {
                        Ok(Some(tool)) => agent.tools.push(tool.as_definition()),
                        _ => missing.push(tn.to_string()),
                    }
                }
            }
        }

        // Make it a deep agent by default when it has tools (so it can call them).
        let deep = params
            .get("deep")
            .and_then(|v| v.as_bool())
            .unwrap_or(!agent.tools.is_empty());
        if deep {
            agent.deep_agent_config = Some(DeepAgentConfig {
                max_iterations: 10,
                enable_reflection: true,
                available_tools: agent.tools.iter().map(|t| t.name.clone()).collect(),
                stop_conditions: vec!["task_complete".to_string()],
                allow_sub_agents: false,
                subagent_spawn_message: None,
            });
        }

        let rationale = params
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let proposal = Proposal::new(ProposalAction::CreateAgent(agent), rationale, parent.name.clone());
        let short: String = proposal.id.chars().take(8).collect();
        let miss = if missing.is_empty() {
            String::new()
        } else {
            format!(
                " NOTE: these requested tools don't exist yet and were skipped: {} — propose/approve them first.",
                missing.join(", ")
            )
        };
        match self.storage.create_proposal(&proposal).await {
            Ok(_) => Some(format!(
                "Proposal {} created: {} (risk: {:?}).{} IMPORTANT: this is NOT applied — the user must approve it. \
                 Do not claim the agent exists. Tell the user what you proposed and that it awaits approval (`agenta approve {}`).",
                short, proposal.summary(), proposal.risk, miss, short,
            )),
            Err(e) => Some(format!("Error creating proposal: {}", e)),
        }
    }

    /// Propose attaching (attach=true) or detaching a knowledge base to/from an
    /// existing agent. Verifies the agent exists here; the KB's existence is checked
    /// at apply time. Drafts a proposal — does not mutate.
    /// Propose a revision to an existing tool.
    ///
    /// Patch semantics on purpose: we start from the tool as it exists and apply
    /// only the fields given, so fixing one thing (say, adding a header) can't
    /// silently drop the schema or the secret allowlist. The proposal still
    /// carries the fully-resolved tool, so what the user approves is exactly what
    /// gets written.
    async fn builtin_propose_update_tool(
        &self,
        parent: &Agent,
        params: &serde_json::Value,
    ) -> Option<String> {
        let name = match params
            .get("tool")
            .or_else(|| params.get("name"))
            .and_then(|v| v.as_str())
        {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => {
                return Some(
                    "Error: propose_update_tool requires a non-empty 'tool' (the tool's name)."
                        .into(),
                )
            }
        };

        let existing = match self.storage.get_tool_by_name(&name).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                return Some(format!(
                    "Tool '{}' not found — call list_tools for the real names on this machine.",
                    name
                ))
            }
            Err(e) => return Some(format!("Error looking up tool: {}", e)),
        };

        let mut next = existing.clone();
        let mut changed: Vec<&str> = Vec::new();

        if let Some(d) = params.get("description").and_then(|v| v.as_str()) {
            if !d.trim().is_empty() {
                next.description = d.to_string();
                changed.push("description");
            }
        }
        if let Some(h) = params.get("handler").and_then(|v| v.as_str()) {
            if !h.trim().is_empty() {
                next.handler = Some(h.to_string());
                changed.push("handler");
            }
        }
        if let Some(p) = params.get("parameters").filter(|p| !p.is_null()) {
            next.parameters = p.clone();
            changed.push("parameters");
        }
        if let Some(secrets) = params.get("secrets").and_then(|v| v.as_array()) {
            next.secrets = secrets
                .iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect();
            changed.push("secrets");
        }
        if let Some(se) = params.get("side_effect").and_then(|v| v.as_str()) {
            next.side_effect = match se.to_lowercase().replace('-', "_").as_str() {
                "write" => SideEffect::Write,
                "destructive" => SideEffect::Destructive,
                _ => SideEffect::ReadOnly,
            };
            changed.push("side_effect");
        }
        if let Some(http) = params.get("http").filter(|h| !h.is_null()) {
            next.http = serde_json::from_value(http.clone()).ok();
            changed.push("http");
        }

        if changed.is_empty() {
            return Some(
                "Error: propose_update_tool needs at least one field to change \
                 (handler, description, parameters, secrets, side_effect or http)."
                    .into(),
            );
        }

        let rationale = params
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let action = ProposalAction::UpdateTool {
            previous_name: name.clone(),
            tool: next,
        };
        let proposal = Proposal::new(action, rationale, parent.name.clone());
        let id: String = proposal.id.chars().take(8).collect();
        match self.storage.create_proposal(&proposal).await {
            Ok(_) => Some(format!(
                "Proposal {} created: update tool '{}' ({}). Tell the user to run `agenta approve {}` (or reject {}).",
                id, name, changed.join(" + "), id, id
            )),
            Err(e) => Some(format!("Error creating proposal: {}", e)),
        }
    }

    /// Propose a revision to an existing agent. Deliberately limited to
    /// prompt/description/model: MIND can refine an agent it built, but there is
    /// no delete path — the worst an approved proposal can do is reword it.
    async fn builtin_propose_update_agent(
        &self,
        parent: &Agent,
        params: &serde_json::Value,
    ) -> Option<String> {
        let agent = match params.get("agent").and_then(|v| v.as_str()) {
            Some(a) if !a.trim().is_empty() => a.trim().to_string(),
            _ => {
                return Some(
                    "Error: propose_update_agent requires a non-empty 'agent' (the name).".into(),
                )
            }
        };

        match self.storage.get_agent_by_name(&agent).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Some(format!(
                    "Agent '{}' not found — call list_agents for the real names on this machine.",
                    agent
                ))
            }
            Err(e) => return Some(format!("Error looking up agent: {}", e)),
        }

        let field = |k: &str| {
            params
                .get(k)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty())
        };
        let system_prompt = field("system_prompt");
        let description = field("description");
        let model = field("model");

        if system_prompt.is_none() && description.is_none() && model.is_none() {
            return Some(
                "Error: propose_update_agent needs at least one of 'system_prompt', \
                 'description' or 'model' to change."
                    .into(),
            );
        }

        let action = ProposalAction::UpdateAgent {
            agent: agent.clone(),
            system_prompt,
            description,
            model,
        };
        let rationale = params
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let proposal = Proposal::new(action, rationale, parent.name.clone());
        let id: String = proposal.id.chars().take(8).collect();
        match self.storage.create_proposal(&proposal).await {
            Ok(_) => Some(format!(
                "Proposal {} created: update agent '{}'. Tell the user to run `agenta approve {}` (or reject {}).",
                id, agent, id, id
            )),
            Err(e) => Some(format!("Error creating proposal: {}", e)),
        }
    }

    async fn builtin_propose_kb(
        &self,
        parent: &Agent,
        params: &serde_json::Value,
        attach: bool,
    ) -> Option<String> {
        let verb = if attach { "attach" } else { "detach" };
        let agent = match params.get("agent").and_then(|v| v.as_str()) {
            Some(a) if !a.trim().is_empty() => a.trim().to_string(),
            _ => return Some(format!("Error: propose_{}_kb requires a non-empty 'agent'.", verb)),
        };
        let kb = match params.get("kb").and_then(|v| v.as_str()) {
            Some(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => return Some(format!("Error: propose_{}_kb requires a non-empty 'kb'.", verb)),
        };
        // Don't let MIND propose against an agent that doesn't exist.
        match self.storage.get_agent_by_name(&agent).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Some(format!(
                    "Agent '{}' not found — call list_agents for the real names on this machine.",
                    agent
                ))
            }
            Err(e) => return Some(format!("Error looking up agent: {}", e)),
        }
        let action = if attach {
            ProposalAction::AttachKb { agent: agent.clone(), kb: kb.clone() }
        } else {
            ProposalAction::DetachKb { agent: agent.clone(), kb: kb.clone() }
        };
        let rationale = params
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let proposal = Proposal::new(action, rationale, parent.name.clone());
        let id: String = proposal.id.chars().take(8).collect();
        match self.storage.create_proposal(&proposal).await {
            Ok(_) => Some(format!(
                "Proposal {} created: {} kb '{}' {} agent '{}'. Tell the user to run `agenta approve {}` (or reject {}).",
                id, verb, kb, if attach { "to" } else { "from" }, agent, id, id
            )),
            Err(e) => Some(format!("Error creating proposal: {}", e)),
        }
    }

    /// Check whether an external command/interpreter is installed & on PATH in the
    /// environment tools actually run in (the daemon copies its PATH into every
    /// tool's sealed env, and this builtin runs in the daemon — so the answer here
    /// predicts whether a script tool that calls the command will work). Lets MIND
    /// verify a dependency BEFORE building a tool around it.
    async fn builtin_check_command(&self, params: &serde_json::Value) -> Option<String> {
        let cmd = match params
            .get("command")
            .or_else(|| params.get("name"))
            .and_then(|v| v.as_str())
        {
            Some(c) if !c.trim().is_empty() => c.trim().to_string(),
            _ => return Some("Error: check_command requires a non-empty 'command'.".to_string()),
        };
        // Only a bare command name or path — reject shell metacharacters/args.
        if cmd.contains(|c: char| c.is_whitespace() || "|&;<>()$`\\\"'*?[]{}".contains(c)) {
            return Some(format!("Error: '{}' is not a plain command name.", cmd));
        }
        match tokio::process::Command::new("which").arg(&cmd).output().await {
            Ok(out) if out.status.success() => {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                Some(format!("FOUND: '{}' is installed at {} — a tool that calls it will work here.", cmd, path))
            }
            Ok(_) => Some(format!(
                "NOT FOUND: '{}' is not installed / not on PATH in this environment, so a tool that calls it WOULD FAIL. Do not build a tool depending on it — tell the user it isn't available and offer an HTTP alternative, or ask them to install it or give the full path.",
                cmd
            )),
            Err(e) => Some(format!("Could not check '{}' (which unavailable?): {}", cmd, e)),
        }
    }

    /// Persist a piece of durable user feedback so it shapes this agent's behavior
    /// on future runs (injected into the prompt at run time). Low-risk → saved
    /// directly, no proposal gate. Scoped to the running agent (parent.name).
    async fn builtin_remember_feedback(
        &self,
        parent: &Agent,
        params: &serde_json::Value,
    ) -> Option<String> {
        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.trim().is_empty() => c.trim().to_string(),
            _ => return Some("Error: remember_feedback requires a non-empty 'content'.".to_string()),
        };
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("note")
            .to_string();
        let mem = crate::core::Memory::new(parent.name.clone(), kind, content.clone());
        match self.storage.add_memory(&mem).await {
            Ok(_)  => Some(format!("Saved to memory — I'll honor this from now on: \"{}\"", content)),
            Err(e) => Some(format!("Error saving feedback: {}", e)),
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
                // execute_ephemeral is a single tool-LESS LLM call. If the spawned
                // agent actually has tools, they did NOT run here — so whatever it
                // "produced" (searches, sends, file writes) is hallucinated. Label
                // the result so neither the caller (e.g. MIND) nor the user mistakes
                // this tool-less preview for a real execution.
                Ok(out) if !named.tools.is_empty() => {
                    let tool_names: Vec<&str> =
                        named.tools.iter().map(|t| t.name.as_str()).collect();
                    Some(format!(
                        "⚠️ Preview only — this was a single LLM call with NO tools executed. \
\"{}\" has tools ({}) that did NOT run, so any searching/sending/writing described below \
did not actually happen. For a real run: agenta run {} --input \"…\"\n\n{}",
                        agent_name,
                        tool_names.join(", "),
                        agent_name,
                        out
                    ))
                }
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
