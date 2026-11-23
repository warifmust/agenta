use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::core::McpServerConfig;

use super::{
    expand_env_value, extract_tool_call_text, parse_tools_list, McpTool, McpTransportClient,
    MCP_PROTOCOL_VERSION,
};

/// Streamable HTTP MCP client (`POST` JSON-RPC 2.0, supports both JSON and SSE responses).
pub struct HttpMcpClient {
    name: String,
    url: String,
    headers: HashMap<String, String>,
    http: reqwest::Client,
    session_id: RwLock<Option<String>>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl HttpMcpClient {
    pub async fn connect(name: String, cfg: &McpServerConfig) -> anyhow::Result<Self> {
        let url = cfg
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("MCP http server '{}' is missing 'url'", name))?
            .to_string();

        let headers: HashMap<String, String> = cfg
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), expand_env_value(v)))
            .collect();

        let client = Self {
            name,
            url,
            headers,
            http: reqwest::Client::new(),
            session_id: RwLock::new(None),
            next_id: AtomicU64::new(1),
            timeout: Duration::from_secs(cfg.timeout_secs.max(1)),
        };

        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&self) -> anyhow::Result<()> {
        let init_params = serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "agenta",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });
        let _ = self.request("initialize", init_params).await?;
        // Best-effort initialized notification (some stateless HTTP servers return 202/204).
        let _ = self
            .notify("notifications/initialized", serde_json::json!({}))
            .await;
        Ok(())
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut req = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .timeout(self.timeout);

        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(sid) = self.session_id.read().await.as_deref() {
            req = req.header("Mcp-Session-Id", sid);
        }

        let _ = req.json(&payload).send().await?;
        Ok(())
    }

    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut req = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .timeout(self.timeout);

        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(sid) = self.session_id.read().await.as_deref() {
            req = req.header("Mcp-Session-Id", sid);
        }

        let resp = req.json(&payload).send().await.map_err(|e| {
            anyhow::anyhow!("MCP http '{}' request '{}' failed: {}", self.name, method, e)
        })?;

        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.write().await = Some(sid.to_string());
        }

        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            anyhow::bail!(
                "MCP http '{}' returned {} on {}: {}",
                self.name,
                status,
                method,
                &body[..body.len().min(300)]
            );
        }

        let parsed = if content_type.contains("text/event-stream") {
            parse_sse_jsonrpc(&body, id)?
        } else {
            serde_json::from_str::<serde_json::Value>(&body).map_err(|e| {
                anyhow::anyhow!("MCP http '{}' invalid JSON on {}: {}", self.name, method, e)
            })?
        };

        if let Some(err) = parsed.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown JSON-RPC error");
            anyhow::bail!("MCP '{}' error on {}: {}", self.name, method, msg);
        }

        Ok(parsed
            .get("result")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})))
    }
}

/// Extract a JSON-RPC response matching `expected_id` from an SSE (`text/event-stream`) body.
pub(crate) fn parse_sse_jsonrpc(
    body: &str,
    expected_id: u64,
) -> anyhow::Result<serde_json::Value> {
    let mut last_valid: Option<serde_json::Value> = None;
    for line in body.lines() {
        let trimmed = line.trim();
        let Some(data) = trimmed.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
            if val.get("id").and_then(|v| v.as_u64()) == Some(expected_id) {
                return Ok(val);
            }
            if val.get("result").is_some() || val.get("error").is_some() {
                last_valid = Some(val);
            }
        }
    }
    last_valid.ok_or_else(|| anyhow::anyhow!("No JSON-RPC response found in SSE stream"))
}

#[async_trait]
impl McpTransportClient for HttpMcpClient {
    async fn list_tools(&self) -> anyhow::Result<Vec<McpTool>> {
        let mut all_tools = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            let result = self.request("tools/list", params).await?;
            let (tools, next_cursor) = parse_tools_list(&result);
            all_tools.extend(tools);
            match next_cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => break,
            }
        }
        Ok(all_tools)
    }

    async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> anyhow::Result<String> {
        let result = self
            .request(
                "tools/call",
                serde_json::json!({
                    "name": tool_name,
                    "arguments": arguments,
                }),
            )
            .await?;
        Ok(extract_tool_call_text(&result))
    }

    async fn is_alive(&self) -> bool {
        true
    }

    async fn shutdown(&self) {}
}
