use anyhow::{anyhow, Result};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::core::Agent;

pub struct ToolInvocation {
    pub name: String,
    pub parameters: serde_json::Value,
}

/// Expand a leading `~` to the real home directory.
/// Needed because we invoke handlers directly without a shell, so `~` is not
/// expanded by the OS.
fn expand_tilde(s: &str) -> String {
    if s.starts_with("~/") || s == "~" {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), &s[1..]);
        }
    }
    s.to_string()
}

pub async fn run_tool(
    agent: &Agent,
    tool_name: &str,
    parameters: serde_json::Value,
) -> Result<String> {
    let tool = agent
        .tools
        .iter()
        .find(|t| t.name == tool_name)
        .ok_or_else(|| anyhow!("Tool not found: {}", tool_name))?;

    let handler = tool
        .handler
        .as_ref()
        .ok_or_else(|| anyhow!("Tool has no handler: {}", tool_name))?;

    let expanded = expand_tilde(handler);
    let mut parts = expanded.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("Invalid tool handler: {}", handler))?;
    let args: Vec<String> = parts.map(|a| expand_tilde(a)).collect();

    let mut child = Command::new(program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("AGENTA_TOOL_NAME", tool_name)
        .env("AGENTA_TOOL_PARAMS", parameters.to_string())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        let payload = parameters.to_string();
        stdin.write_all(payload.as_bytes()).await?;
    }

    let output = child.wait_with_output().await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(anyhow!(
            "Tool {} failed ({}): {}",
            tool_name,
            output.status,
            stderr
        ))
    }
}
