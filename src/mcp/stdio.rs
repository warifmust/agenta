use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, warn};

use crate::core::McpServerConfig;

use super::{
    expand_env_value, extract_tool_call_text, parse_tools_list, McpTool, McpTransportClient,
    MCP_PROTOCOL_VERSION,
};

pub struct StdioMcpClient {
    name: String,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    timeout: Duration,
}

impl StdioMcpClient {
    pub async fn connect(name: String, cfg: &McpServerConfig) -> anyhow::Result<Self> {
        let cmd_str = cfg
            .command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("MCP stdio server '{}' is missing 'command'", name))?;

        let mut cmd = Command::new(cmd_str);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Least-privilege environment: clear inherited secrets, keep only basic
        // runtime vars needed by npx/uvx/python/node, plus explicitly declared env.
        cmd.env_clear();
        for base_var in ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TMPDIR", "XDG_CACHE_HOME"] {
            if let Ok(val) = std::env::var(base_var) {
                cmd.env(base_var, val);
            }
        }
        for (k, v) in &cfg.env {
            cmd.env(k, expand_env_value(v));
        }

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!(
                "Failed to spawn MCP server '{}' ({}): {}",
                name,
                cmd_str,
                e
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stdin for MCP server '{}'", name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to open stdout for MCP server '{}'", name))?;
        let stderr = child.stderr.take();

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));

        // Background stdout reader dispatching JSON-RPC responses by `id`.
        let pending_reader = pending.clone();
        let alive_reader = alive.clone();
        let name_reader = name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                    debug!("[mcp:{}] non-JSON stdout line: {}", name_reader, trimmed);
                    continue;
                };
                if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
                    if let Some(tx) = pending_reader.lock().await.remove(&id) {
                        let _ = tx.send(val);
                    }
                }
            }
            alive_reader.store(false, Ordering::SeqCst);
            // Fail any in-flight waiters immediately rather than waiting for timeout.
            pending_reader.lock().await.clear();
            debug!("[mcp:{}] stdout stream closed", name_reader);
        });

        // Drain stderr so child never blocks on a full pipe.
        if let Some(stderr) = stderr {
            let name_err = name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("[mcp:{} stderr] {}", name_err, line);
                }
            });
        }

        let client = Self {
            name,
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            pending,
            next_id: AtomicU64::new(1),
            alive,
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
        self.notify("notifications/initialized", serde_json::json!({}))
            .await?;
        Ok(())
    }

    async fn notify(&self, method: &str, params: serde_json::Value) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&payload)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        if !self.alive.load(Ordering::SeqCst) {
            anyhow::bail!("MCP server '{}' process is no longer running", self.name);
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&payload)?;
        line.push('\n');

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        {
            let mut stdin = self.stdin.lock().await;
            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                self.pending.lock().await.remove(&id);
                self.alive.store(false, Ordering::SeqCst);
                anyhow::bail!("MCP server '{}' write failed: {}", self.name, e);
            }
            if let Err(e) = stdin.flush().await {
                self.pending.lock().await.remove(&id);
                self.alive.store(false, Ordering::SeqCst);
                anyhow::bail!("MCP server '{}' flush failed: {}", self.name, e);
            }
        }

        let resp = match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(val)) => val,
            Ok(Err(_)) => {
                self.alive.store(false, Ordering::SeqCst);
                anyhow::bail!(
                    "MCP server '{}' closed before responding to '{}'",
                    self.name,
                    method
                );
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                anyhow::bail!(
                    "MCP server '{}' timed out after {}s on '{}'",
                    self.name,
                    self.timeout.as_secs(),
                    method
                );
            }
        };

        if let Some(err) = resp.get("error") {
            let msg = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown JSON-RPC error");
            anyhow::bail!("MCP '{}' error on {}: {}", self.name, method, msg);
        }

        Ok(resp
            .get("result")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})))
    }
}

#[async_trait]
impl McpTransportClient for StdioMcpClient {
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
        self.alive.load(Ordering::SeqCst)
    }

    async fn shutdown(&self) {
        self.alive.store(false, Ordering::SeqCst);
        let mut child = self.child.lock().await;
        if let Err(e) = child.kill().await {
            warn!("[mcp:{}] failed to kill child process: {}", self.name, e);
        }
    }
}
