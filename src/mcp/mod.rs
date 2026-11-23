//! Model Context Protocol (MCP) client support.
//!
//! Servers are configured once globally in `~/.agenta/config.toml` under
//! `[mcp_servers.<name>]` and attached explicitly per-agent via
//! `agent.config.mcp_servers` (default-deny). Each attachment can further
//! scope access with `read_only`, `allow_tools`, and `deny_tools`.

pub mod http;
pub mod stdio;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::core::{AgentMcpRef, McpServerConfig, McpTransport};

pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
pub const MAX_MCP_TOOL_OUTPUT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpToolAnnotations {
    #[serde(rename = "readOnlyHint", default)]
    pub read_only_hint: Option<bool>,
    #[serde(rename = "destructiveHint", default)]
    pub destructive_hint: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub annotations: Option<McpToolAnnotations>,
}

#[async_trait]
pub trait McpTransportClient: Send + Sync {
    async fn list_tools(&self) -> anyhow::Result<Vec<McpTool>>;
    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<String>;
    async fn is_alive(&self) -> bool;
    async fn shutdown(&self);
}

/// Daemon-level pool of MCP server connections.
/// Connections are established lazily on first use and auto-reconnected if a
/// stdio server exits unexpectedly.
pub struct McpManager {
    configs: HashMap<String, McpServerConfig>,
    clients: RwLock<HashMap<String, Arc<dyn McpTransportClient>>>,
    /// Cached `tools/list` results per server so every agent turn doesn't re-query.
    tools_cache: RwLock<HashMap<String, Vec<McpTool>>>,
}

impl McpManager {
    pub fn new(configs: HashMap<String, McpServerConfig>) -> Self {
        Self {
            configs,
            clients: RwLock::new(HashMap::new()),
            tools_cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn has_server(&self, name: &str) -> bool {
        self.configs.contains_key(name)
    }

    pub fn server_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.configs.keys().cloned().collect();
        names.sort();
        names
    }

    async fn get_or_connect(&self, server: &str) -> anyhow::Result<Arc<dyn McpTransportClient>> {
        if let Some(existing) = self.clients.read().await.get(server).cloned() {
            if existing.is_alive().await {
                return Ok(existing);
            }
            warn!("[mcp:{}] connection dropped; reconnecting", server);
        }

        let cfg = self
            .configs
            .get(server)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' is not configured in config.toml", server))?
            .clone();

        let client: Arc<dyn McpTransportClient> = match cfg.transport {
            McpTransport::Stdio => Arc::new(stdio::StdioMcpClient::connect(server.to_string(), &cfg).await?),
            McpTransport::Http => Arc::new(http::HttpMcpClient::connect(server.to_string(), &cfg).await?),
        };

        info!("[mcp:{}] connected ({:?})", server, cfg.transport);
        self.clients
            .write()
            .await
            .insert(server.to_string(), client.clone());
        Ok(client)
    }

    /// Fetch all tools exposed by `server` (cached until `refresh = true` or reconnect).
    pub async fn list_tools(&self, server: &str, refresh: bool) -> anyhow::Result<Vec<McpTool>> {
        if !refresh {
            if let Some(cached) = self.tools_cache.read().await.get(server).cloned() {
                if let Some(client) = self.clients.read().await.get(server) {
                    if client.is_alive().await {
                        return Ok(cached);
                    }
                }
            }
        }

        let client = self.get_or_connect(server).await?;
        let tools = client.list_tools().await?;
        self.tools_cache
            .write()
            .await
            .insert(server.to_string(), tools.clone());
        Ok(tools)
    }

    /// Resolve all permitted MCP tools for an agent's `mcp_servers` attachments.
    /// Gracefully skips any server that is unreachable (logs a warning) so one
    /// flaky upstream doesn't block the agent's other MCP servers.
    pub async fn tools_for_agent(
        &self,
        refs: &[AgentMcpRef],
    ) -> Vec<(String, McpTool)> {
        let mut out = Vec::new();
        for mcp_ref in refs {
            match self.list_tools(&mcp_ref.server, false).await {
                Ok(tools) => {
                    for tool in filter_tools(&tools, mcp_ref) {
                        out.push((mcp_ref.server.clone(), tool));
                    }
                }
                Err(e) => {
                    warn!(
                        "[mcp:{}] failed to list tools (skipping for this run): {}",
                        mcp_ref.server, e
                    );
                }
            }
        }
        out
    }

    /// Call a tool on `server`, capping the returned output size to keep context safe.
    pub async fn call_tool(
        &self,
        server: &str,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<String> {
        let client = self.get_or_connect(server).await?;
        let raw = client.call_tool(tool_name, arguments).await?;
        Ok(truncate_output(&raw, MAX_MCP_TOOL_OUTPUT_BYTES))
    }

    /// Gracefully shut down all running MCP child processes.
    pub async fn shutdown_all(&self) {
        let clients: Vec<(String, Arc<dyn McpTransportClient>)> =
            self.clients.write().await.drain().collect();
        for (name, client) in clients {
            info!("[mcp:{}] shutting down", name);
            client.shutdown().await;
        }
    }
}

// ── Naming, filtering & prompt formatting helpers ─────────────────────────────

/// Format an MCP tool name for the LLM tool catalogue: `mcp__<server>__<tool>`.
pub fn format_mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", server, tool)
}

/// Check if a tool name is an MCP-namespaced tool (`mcp__<server>__<tool>`).
pub fn is_mcp_tool_name(name: &str) -> bool {
    name.starts_with("mcp__")
}

/// Filter a server's tools according to an agent's `AgentMcpRef` policy.
pub fn filter_tools(tools: &[McpTool], policy: &AgentMcpRef) -> Vec<McpTool> {
    tools
        .iter()
        .filter(|t| {
            if !policy.allow_tools.is_empty()
                && !policy.allow_tools.iter().any(|a| a == &t.name)
            {
                return false;
            }
            if policy.deny_tools.iter().any(|d| d == &t.name) {
                return false;
            }
            if policy.read_only && !is_tool_read_only(t) {
                return false;
            }
            true
        })
        .cloned()
        .collect()
}

/// Determine whether an MCP tool is read-only.
/// 1. Honors MCP `annotations.readOnlyHint` if explicitly provided.
/// 2. Rejects if `annotations.destructiveHint == Some(true)`.
/// 3. Falls back to a conservative name-based heuristic when annotations are absent.
pub fn is_tool_read_only(tool: &McpTool) -> bool {
    if let Some(ann) = &tool.annotations {
        if let Some(ro) = ann.read_only_hint {
            return ro;
        }
        if ann.destructive_hint == Some(true) {
            return false;
        }
    }

    let lower = tool.name.to_ascii_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| c == '_' || c == '-' || c == '.' || c == '/')
        .filter(|s| !s.is_empty())
        .collect();

    const WRITE_VERBS: &[&str] = &[
        "create", "update", "delete", "remove", "destroy", "edit", "modify", "write",
        "put", "post", "patch", "set", "add", "append", "insert", "upsert",
        "merge", "push", "commit", "fork", "close", "reopen", "assign", "unassign",
        "comment", "reply", "transition", "approve", "dismiss", "cancel", "trigger",
        "run", "exec", "execute", "send", "publish", "archive", "link", "unlink",
    ];

    !tokens.iter().any(|tok| WRITE_VERBS.contains(tok))
}

/// Build a compact single-line description for the agent's system prompt,
/// including parameter names/types (with `*` for required params) without
/// bloating the context with full JSON Schemas.
pub fn format_tool_prompt_line(server: &str, tool: &McpTool) -> String {
    let full_name = format_mcp_tool_name(server, &tool.name);
    let desc = tool
        .description
        .as_deref()
        .unwrap_or("MCP tool")
        .lines()
        .next()
        .unwrap_or("MCP tool")
        .trim();
    let params_hint = compact_schema_hint(&tool.input_schema);
    if params_hint.is_empty() {
        format!("- {} (MCP: {}): {}", full_name, server, desc)
    } else {
        format!("- {} (MCP: {}): {} — params: {}", full_name, server, desc, params_hint)
    }
}

/// Render `{ "properties": { "q": { "type": "string" }, ... }, "required": ["q"] }`
/// as `(q*: string, limit: integer)`.
pub fn compact_schema_hint(schema: &serde_json::Value) -> String {
    let Some(props) = schema.get("properties").and_then(|v| v.as_object()) else {
        return String::new();
    };
    if props.is_empty() {
        return String::new();
    }

    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut keys: Vec<&String> = props.keys().collect();
    keys.sort();

    let parts: Vec<String> = keys
        .into_iter()
        .take(12)
        .map(|k| {
            let ty = props
                .get(k)
                .and_then(|v| v.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("any");
            let star = if required.contains(&k.as_str()) { "*" } else { "" };
            format!("{}{}: {}", k, star, ty)
        })
        .collect();

    format!("({})", parts.join(", "))
}

/// Expand `$VAR` (whole string) or `${VAR}` placeholders from the process environment.
pub fn expand_env_value(raw: &str) -> String {
    if let Some(var) = raw.strip_prefix('$') {
        if !var.starts_with('{') && var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return std::env::var(var).unwrap_or_default();
        }
    }
    // Support inline `${VAR}` or `Bearer $TOKEN` patterns.
    let mut out = raw.to_string();
    if let Ok(re_braced) = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}") {
        out = re_braced
            .replace_all(&out, |caps: &regex::Captures| {
                std::env::var(&caps[1]).unwrap_or_default()
            })
            .into_owned();
    }
    if let Ok(re_bare) = Regex::new(r"\$([A-Za-z_][A-Za-z0-9_]*)") {
        out = re_bare
            .replace_all(&out, |caps: &regex::Captures| {
                std::env::var(&caps[1]).unwrap_or_default()
            })
            .into_owned();
    }
    out
}

pub(crate) fn parse_tools_list(result: &serde_json::Value) -> (Vec<McpTool>, Option<String>) {
    let tools: Vec<McpTool> = result
        .get("tools")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let next_cursor = result
        .get("nextCursor")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (tools, next_cursor)
}

pub(crate) fn extract_tool_call_text(result: &serde_json::Value) -> String {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let text = if let Some(content) = result.get("content").and_then(|v| v.as_array()) {
        let parts: Vec<String> = content
            .iter()
            .filter_map(|item| {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    Some(t.to_string())
                } else {
                    serde_json::to_string(item).ok()
                }
            })
            .collect();
        parts.join("\n")
    } else {
        serde_json::to_string_pretty(result).unwrap_or_default()
    };

    if is_error {
        format!("MCP tool returned error: {}", text)
    } else if text.is_empty() {
        "MCP tool returned empty output.".to_string()
    } else {
        text
    }
}

fn truncate_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n... [truncated {} bytes]",
        &s[..end],
        s.len() - end
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mk_tool(name: &str, ro_hint: Option<bool>) -> McpTool {
        McpTool {
            name: name.to_string(),
            description: Some(format!("Desc for {}", name)),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["query"]
            }),
            annotations: ro_hint.map(|h| McpToolAnnotations {
                read_only_hint: Some(h),
                destructive_hint: None,
            }),
        }
    }

    #[test]
    fn filter_tools_enforces_read_only_allow_and_deny() {
        let tools = vec![
            mk_tool("search_issues", None),
            mk_tool("get_pull_request", None),
            mk_tool("create_issue", None),
            mk_tool("delete_branch", None),
            mk_tool("custom_query", Some(true)),
            mk_tool("sneaky_read", Some(false)),
        ];

        // read_only = true filters out create/delete and readOnlyHint=false
        let ro_policy = AgentMcpRef {
            server: "github".into(),
            read_only: true,
            allow_tools: vec![],
            deny_tools: vec![],
        };
        let names: Vec<String> = filter_tools(&tools, &ro_policy)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["search_issues", "get_pull_request", "custom_query"]);

        // allow_tools + deny_tools
        let scoped = AgentMcpRef {
            server: "github".into(),
            read_only: false,
            allow_tools: vec!["search_issues".into(), "create_issue".into()],
            deny_tools: vec!["create_issue".into()],
        };
        let names2: Vec<String> = filter_tools(&tools, &scoped)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names2, vec!["search_issues"]);
    }

    #[test]
    fn compact_schema_hint_formats_required_and_optional_params() {
        let t = mk_tool("search_issues", None);
        let line = format_tool_prompt_line("github", &t);
        assert!(line.contains("mcp__github__search_issues"));
        assert!(line.contains("(limit: integer, query*: string)"));
    }

    #[test]
    fn expand_env_value_supports_bare_and_inline_vars() {
        std::env::set_var("AGENTA_MCP_TEST_TOK", "secret123");
        assert_eq!(expand_env_value("$AGENTA_MCP_TEST_TOK"), "secret123");
        assert_eq!(
            expand_env_value("Bearer $AGENTA_MCP_TEST_TOK"),
            "Bearer secret123"
        );
        assert_eq!(
            expand_env_value("Bearer ${AGENTA_MCP_TEST_TOK}"),
            "Bearer secret123"
        );
        std::env::remove_var("AGENTA_MCP_TEST_TOK");
    }

    #[test]
    fn sse_parser_extracts_matching_jsonrpc_response() {
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n";
        let val = http::parse_sse_jsonrpc(sse, 2).expect("parse SSE");
        assert_eq!(val["id"], 2);
        assert!(val["result"]["tools"].is_array());
    }

    #[tokio::test]
    async fn stdio_roundtrip_with_mock_mcp_server() {
        // Tiny python3 mock MCP server over stdio to verify initialize -> tools/list -> tools/call.
        let script = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        resp = {"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}}}
    elif method == "tools/list":
        resp = {"jsonrpc": "2.0", "id": mid, "result": {"tools": [{"name": "echo_issue", "description": "Echo a Jira issue key", "inputSchema": {"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]}}]}}
    elif method == "tools/call":
        key = msg["params"]["arguments"].get("key", "")
        resp = {"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": f"Issue {key}: In Progress"}]}}
    else:
        continue
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;
        let mut servers = HashMap::new();
        servers.insert(
            "mock".to_string(),
            McpServerConfig {
                transport: McpTransport::Stdio,
                command: Some("python3".to_string()),
                args: vec!["-u".to_string(), "-c".to_string(), script.to_string()],
                env: HashMap::new(),
                url: None,
                headers: HashMap::new(),
                timeout_secs: 10,
            },
        );

        let mgr = McpManager::new(servers);
        let tools = mgr.list_tools("mock", false).await.expect("list_tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo_issue");

        let out = mgr
            .call_tool("mock", "echo_issue", json!({"key": "PROJ-42"}))
            .await
            .expect("call_tool");
        assert_eq!(out, "Issue PROJ-42: In Progress");

        mgr.shutdown_all().await;
    }
}
