use anyhow::{anyhow, Result};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::core::Agent;

pub struct ToolInvocation {
    pub name: String,
    pub parameters: serde_json::Value,
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

    let mut parts = handler.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("Invalid tool handler: {}", handler))?;
    let args: Vec<&str> = parts.collect();

    let mut child = Command::new(program)
        .args(args)
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
