use anyhow::{anyhow, Result};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::core::Agent;

pub struct ToolInvocation {
    pub name: String,
    pub parameters: serde_json::Value,
}

/// Names of tools handled natively by the runtime — no external script needed.
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "spawn_agent",
    "read_file",
    "write_file",
    "list_files",
];

pub fn is_builtin_tool(name: &str) -> bool {
    BUILTIN_TOOL_NAMES.contains(&name)
}

/// Descriptions injected into every agent's system prompt.
pub fn builtin_tool_descriptions() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "read_file",
            "Read the contents of a file. \
             Parameters: {\"path\": \"<file path>\"}",
        ),
        (
            "write_file",
            "Write content to a file, creating it if it doesn't exist. \
             Parameters: {\"path\": \"<file path>\", \"content\": \"<text to write>\"}",
        ),
        (
            "list_files",
            "List files in a directory. \
             Parameters: {\"path\": \"<directory path>\", \"pattern\": \"<optional glob, e.g. *.md>\"}",
        ),
        (
            "spawn_agent",
            "Spawn a sub-agent to handle a specific sub-task and return its output. \
             Use `name` to delegate to an existing named agent (e.g. CORAL, WILL), \
             or `role` to spin up a throwaway agent with a custom system prompt. \
             Parameters: {\"name\": \"<existing agent name OR omit>\", \
             \"role\": \"<system prompt if no name>\", \
             \"input\": \"<task>\", \
             \"model\": \"<optional — defaults to caller model>\"}",
        ),
    ]
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
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = match (stdout.is_empty(), stderr.is_empty()) {
            (false, _)   => stdout,
            (true, false) => stderr,
            (true, true)  => String::new(),
        };
        Err(anyhow!(
            "Tool {} failed ({}): {}",
            tool_name,
            output.status,
            detail
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_tools_are_builtin() {
        assert!(is_builtin_tool("read_file"));
        assert!(is_builtin_tool("write_file"));
        assert!(is_builtin_tool("list_files"));
    }

    #[test]
    fn spawn_agent_is_builtin() {
        assert!(is_builtin_tool("spawn_agent"));
    }

    #[test]
    fn unknown_tool_is_not_builtin() {
        assert!(!is_builtin_tool("tavily_search"));
        assert!(!is_builtin_tool(""));
        assert!(!is_builtin_tool("read_file_v2"));
    }

    #[test]
    fn builtin_tool_descriptions_covers_all_builtins() {
        let descs = builtin_tool_descriptions();
        let names: Vec<&str> = descs.iter().map(|(n, _)| *n).collect();
        for builtin in BUILTIN_TOOL_NAMES {
            assert!(
                names.contains(builtin),
                "missing description for built-in tool: {}", builtin
            );
        }
    }

    #[test]
    fn builtin_tool_descriptions_are_non_empty() {
        for (name, desc) in builtin_tool_descriptions() {
            assert!(!name.is_empty(), "tool name should not be empty");
            assert!(!desc.is_empty(), "description for '{}' should not be empty", name);
        }
    }
}
