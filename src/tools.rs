use anyhow::{anyhow, Result};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::core::Agent;

/// Wall-clock cap for a single tool handler. A hung handler (stalled network,
/// a `read` waiting on input, an infinite loop) would otherwise wedge the agent
/// indefinitely. Overridable per-deployment via `AGENTA_TOOL_TIMEOUT_SECS`.
const DEFAULT_TOOL_TIMEOUT_SECS: u64 = 120;

/// Environment variables always passed through to a handler so interpreters
/// resolve (`/usr/bin/env bash …` needs PATH, `~` needs HOME, etc.). Everything
/// else — including every agent secret — is withheld unless the tool allowlists it.
const BASELINE_ENV: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL", "TERM", "TMPDIR", "SHELL"];

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
    "list_tools",
    "list_agents",
    "get_tool",
    "get_agent",
    "propose_create_tool",
    "propose_create_agent",
    "propose_attach_kb",
    "propose_detach_kb",
    "remember_feedback",
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
             Use `name` to delegate to an EXISTING named agent — call list_agents \
             first to get the real names on THIS machine; never guess or invent an \
             agent name. Or use `role` to spin up a throwaway agent with a custom system prompt. \
             Parameters: {\"name\": \"<existing agent name OR omit>\", \
             \"role\": \"<system prompt if no name>\", \
             \"input\": \"<task>\", \
             \"model\": \"<optional — defaults to caller model>\"}",
        ),
        (
            "list_tools",
            "List every tool that exists in agenta (name, type, side-effect, description). \
             Use this to answer \"what tools exist\" and to avoid proposing a duplicate. No parameters.",
        ),
        (
            "list_agents",
            "List every agent in agenta (name, model, status, attached knowledge bases). \
             Use this to answer \"what agents exist\". No parameters.",
        ),
        (
            "get_tool",
            "Get one tool's full details (schema, handler, secrets, side-effect). \
             Parameters: {\"name\": \"<tool name>\"}",
        ),
        (
            "get_agent",
            "Get one agent's details (model, status, tools, knowledge bases, system prompt). \
             Parameters: {\"name\": \"<agent name>\"}",
        ),
        (
            "propose_create_tool",
            "Propose creating a new agenta tool. This does NOT create it — it drafts a \
             proposal the user must approve first, so never claim the tool exists after calling this. \
             Prefer this over writing tool files by hand. \
             Parameters: {\"name\": \"<tool name>\", \"description\": \"<what it does>\", \
             \"parameters\": <JSON schema of the tool's inputs>, \
             \"handler\": \"<command, e.g. /usr/bin/env bash /path/tool.sh — OMIT for an http tool>\", \
             \"http\": {\"method\": \"POST\", \"headers\": {\"Authorization\": \"Bearer ${SECRET_NAME}\"}} \
             (for API tools; then `handler` is the URL), \
             \"secrets\": [\"ENV_VAR_NAME\"], \
             \"side_effect\": \"read_only|write|destructive\", \
             \"rationale\": \"<why you are proposing this>\"}",
        ),
        (
            "propose_create_agent",
            "Propose creating a new agenta agent (a persistent worker the user can run). This does NOT \
             create it — it drafts a proposal the user must approve; never claim the agent exists. \
             Parameters: {\"name\": \"<AGENT_NAME>\", \
             \"model\": \"<model id — OMIT to reuse your own model>\", \
             \"provider\": \"<optional, e.g. openrouter>\", \
             \"system_prompt\": \"<the agent's instructions/persona>\", \
             \"tools\": [\"<existing tool name>\"] (the tools it may call — they must already exist), \
             \"deep\": true (multi-step tool-using agent; defaults true when it has tools), \
             \"description\": \"<optional>\", \"rationale\": \"<why>\"}. \
             If the user asks for BOTH a tool and an agent that uses it, propose the tool first and have \
             the user approve it, then propose the agent referencing that tool by name.",
        ),
        (
            "propose_attach_kb",
            "Propose attaching a knowledge base (RAG) to an existing agent, so that agent retrieves from \
             it. Drafts a proposal the user approves; does NOT attach it directly. \
             Parameters: {\"agent\": \"<existing agent name>\", \"kb\": \"<knowledge base name>\", \
             \"rationale\": \"<why>\"}. Use the agent name from list_agents and the KB name the user gave \
             you (the KB is validated when the proposal is applied).",
        ),
        (
            "propose_detach_kb",
            "Propose detaching a knowledge base from an existing agent (reverse of propose_attach_kb). \
             Drafts a proposal the user approves. \
             Parameters: {\"agent\": \"<existing agent name>\", \"kb\": \"<knowledge base name>\", \"rationale\": \"<why>\"}.",
        ),
        (
            "remember_feedback",
            "Save a durable piece of feedback, correction, or preference the user has given you, so you \
             honor it on every future run (it gets injected into your instructions). Call this whenever \
             the user tells you how they want you to behave going forward — e.g. \"always X\", \"stop \
             doing Y\", \"from now on Z\", or corrects you in a way that should stick. \
             Parameters: {\"content\": \"<the rule/preference, phrased so future-you understands it>\", \
             \"kind\": \"preference|correction|note\" (optional)}. Saves immediately (no approval needed).",
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

/// The JSON-type name a value would satisfy in a schema (`integer` reported as
/// `number` since JSON has no separate integer type; handled leniently below).
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Whether a value satisfies a schema `type` string. Lenient where JSON is:
/// `integer` accepts whole-valued numbers, `number` accepts any number.
fn matches_schema_type(expected: &str, value: &serde_json::Value) -> bool {
    match expected {
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        other => json_type_name(value) == other,
    }
}

/// Lightweight validation of model-supplied `args` against a tool's JSON-Schema
/// `parameters`. Deliberately permissive: it only enforces what the schema
/// clearly states — presence of `required` fields and the declared `type` of
/// any provided property. Unknown/extra fields and un-typed properties pass.
fn validate_params(
    tool_name: &str,
    schema: &serde_json::Value,
    args: &serde_json::Value,
) -> Result<()> {
    let Some(schema_obj) = schema.as_object() else {
        return Ok(()); // no schema to check against
    };

    // Required fields must be present (and args must therefore be an object).
    if let Some(required) = schema_obj.get("required").and_then(|r| r.as_array()) {
        if !required.is_empty() {
            let obj = args.as_object().ok_or_else(|| {
                anyhow!(
                    "Tool {} expects an object with fields {:?}, got {}",
                    tool_name,
                    required
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>(),
                    json_type_name(args)
                )
            })?;
            for field in required.iter().filter_map(|v| v.as_str()) {
                if !obj.contains_key(field) {
                    return Err(anyhow!(
                        "Tool {} missing required parameter '{}'",
                        tool_name,
                        field
                    ));
                }
            }
        }
    }

    // Type-check any provided property that declares a `type`.
    if let (Some(props), Some(obj)) = (
        schema_obj.get("properties").and_then(|p| p.as_object()),
        args.as_object(),
    ) {
        for (key, value) in obj {
            if value.is_null() {
                continue; // treat explicit null as "unset"
            }
            if let Some(expected) = props
                .get(key)
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
            {
                if !matches_schema_type(expected, value) {
                    return Err(anyhow!(
                        "Tool {} parameter '{}' should be {}, got {}",
                        tool_name,
                        key,
                        expected,
                        json_type_name(value)
                    ));
                }
            }
        }
    }

    Ok(())
}

/// Resolve the tool timeout. Precedence: the tool's own `timeout_secs` (for
/// long-running orchestrators), then the AGENTA_TOOL_TIMEOUT_SECS env override,
/// then the global default.
fn tool_timeout_secs(tool: &crate::core::ToolDefinition) -> u64 {
    if let Some(t) = tool.timeout_secs.filter(|v| *v > 0) {
        return t;
    }
    std::env::var("AGENTA_TOOL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TOOL_TIMEOUT_SECS)
}

/// Substitute `${NAME}` placeholders from the tool's secret allowlist. A
/// placeholder must name an allowlisted secret that is set in the environment —
/// otherwise we error rather than leak arbitrary env or send a literal `${...}`.
fn substitute_vars(input: &str, allowed: &[String]) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow!("Unterminated ${{...}} placeholder in '{}'", input))?;
        let name = &after[..end];
        if !allowed.iter().any(|s| s == name) {
            return Err(anyhow!(
                "Placeholder ${{{}}} is not in the tool's secret allowlist (add it with --secret {})",
                name,
                name
            ));
        }
        let val = std::env::var(name)
            .map_err(|_| anyhow!("Secret '{}' is not set in the environment", name))?;
        out.push_str(&val);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Execute an HTTP-backed tool: build the request from the declared endpoint,
/// interpolate secrets into the URL and headers, send the call parameters as the
/// JSON body (for methods that take one), and return the response body.
async fn run_http_tool(
    tool_name: &str,
    url: &str,
    http: &crate::core::HttpHandler,
    secrets: &[String],
    parameters: &serde_json::Value,
    timeout_secs: u64,
) -> Result<String> {
    let method = reqwest::Method::from_bytes(http.method.to_uppercase().as_bytes())
        .map_err(|_| anyhow!("Invalid HTTP method '{}'", http.method))?;
    let url = substitute_vars(url, secrets)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()?;

    let mut req = client.request(method.clone(), &url);
    for (key, value) in &http.headers {
        req = req.header(key, substitute_vars(value, secrets)?);
    }
    // The call parameters are the request body, mirroring stdin for script tools.
    if !matches!(method, reqwest::Method::GET | reqwest::Method::HEAD) {
        req = req.json(parameters);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| anyhow!("Tool {} HTTP request failed: {}", tool_name, e))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        Ok(body.trim().to_string())
    } else {
        Err(anyhow!(
            "Tool {} failed (HTTP {}): {}",
            tool_name,
            status.as_u16(),
            body.trim()
        ))
    }
}

/// Run a tool the given agent has attached, by name. This is the autonomous
/// entry point (agent execution): it enforces the destructive-tool policy before
/// delegating to the shared executor. The manual `tool run` path guards
/// interactively instead and calls `execute_tool` directly.
pub async fn run_tool(
    agent: &Agent,
    tool_name: &str,
    parameters: serde_json::Value,
) -> Result<String> {
    use crate::core::SideEffect;

    let tool = agent
        .tools
        .iter()
        .find(|t| t.name == tool_name)
        .ok_or_else(|| anyhow!("Tool not found: {}", tool_name))?;

    // Safe-by-default: an irreversible tool won't fire unattended unless the
    // agent explicitly opted in. Write tools are not gated.
    if tool.side_effect == SideEffect::Destructive && !agent.config.allow_destructive_tools {
        return Err(anyhow!(
            "Tool '{}' is destructive and this agent is not allowed to run destructive tools autonomously \
             (set config.allow_destructive_tools = true to permit it)",
            tool_name
        ));
    }

    execute_tool(tool, parameters).await
}

/// The single tool executor. Every path that runs a handler — agent execution
/// and the manual `tool run` path — goes through here, so validation, the secret
/// allowlist, env sealing, and the timeout are applied uniformly. Do not spawn
/// tool handlers anywhere else.
pub async fn execute_tool(
    tool: &crate::core::ToolDefinition,
    parameters: serde_json::Value,
) -> Result<String> {
    let tool_name = tool.name.as_str();

    // Reject obviously-wrong arguments before we spawn anything, so a handler
    // never runs with a missing required field or a mistyped value.
    validate_params(tool_name, &tool.parameters, &parameters)?;

    let handler = tool
        .handler
        .as_ref()
        .ok_or_else(|| anyhow!("Tool has no handler: {}", tool_name))?;

    let timeout_secs = tool_timeout_secs(tool);

    // HTTP tools call an endpoint instead of spawning a process; `handler` is the URL.
    if let Some(http) = &tool.http {
        return run_http_tool(tool_name, handler, http, &tool.secrets, &parameters, timeout_secs).await;
    }

    // Parse the handler with shell-quoting rules so quoted / multi-line inline
    // scripts (e.g. `bash -lc 'python3 - <<PY … PY'`) survive intact —
    // split_whitespace would shred a quoted argument into broken fragments.
    let mut tokens = shlex::split(handler)
        .ok_or_else(|| anyhow!("Invalid tool handler (unbalanced quotes): {}", handler))?
        .into_iter();
    let program = expand_tilde(
        &tokens
            .next()
            .ok_or_else(|| anyhow!("Empty tool handler: {}", handler))?,
    );
    let args: Vec<String> = tokens.map(|a| expand_tilde(&a)).collect();

    let mut command = Command::new(&program);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Kill the handler if it outlives the timeout (the timed-out future is
        // dropped, which drops the Child, which — with this set — kills the process).
        .kill_on_drop(true)
        // Start from an empty environment so a tool can't read secrets it wasn't
        // granted; then add back only what it needs.
        .env_clear();

    // Baseline vars so interpreters/paths resolve.
    for key in BASELINE_ENV {
        if let Ok(val) = std::env::var(key) {
            command.env(key, val);
        }
    }

    command
        .env("AGENTA_TOOL_NAME", tool_name)
        .env("AGENTA_TOOL_PARAMS", parameters.to_string());

    // Inject only the secrets this tool explicitly allowlisted. A declared-but-unset
    // var is simply skipped (the handler can decide how to handle its absence).
    for secret in &tool.secrets {
        if let Ok(val) = std::env::var(secret) {
            command.env(secret, val);
        }
    }

    let mut child = command.spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        let payload = parameters.to_string();
        stdin.write_all(payload.as_bytes()).await?;
    }

    // `timeout_secs` was resolved from the tool up top.
    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(anyhow!(
                "Tool {} timed out after {}s",
                tool_name,
                timeout_secs
            ));
        }
    };
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

    use crate::core::agent::ToolDefinition;

    /// Build a throwaway agent carrying one script-backed tool.
    fn agent_with_tool(tool_name: &str, script: &str, secrets: Vec<String>) -> (Agent, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "agenta_tool_test_{}_{}.sh",
            tool_name,
            std::process::id()
        ));
        std::fs::write(&path, script).unwrap();

        let mut agent = Agent::new("test".into(), "test-model".into(), "sys".into());
        agent.tools = vec![ToolDefinition {
            name: tool_name.into(),
            description: "test tool".into(),
            parameters: serde_json::json!({}),
            handler: Some(format!("/usr/bin/env bash {}", path.display())),
            secrets,
            side_effect: Default::default(),
            http: None,
            timeout_secs: None,
        }];
        (agent, path)
    }

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(f)
    }

    #[test]
    fn only_allowlisted_secrets_reach_the_handler() {
        std::env::set_var("AGENTA_TEST_GRANTED", "yes");
        std::env::set_var("AGENTA_TEST_DENIED", "leak");

        // Handler reports whether each var made it into its environment.
        let script = r#"echo "granted=${AGENTA_TEST_GRANTED:-MISSING} denied=${AGENTA_TEST_DENIED:-MISSING}""#;
        let (agent, path) = agent_with_tool(
            "env_probe",
            script,
            vec!["AGENTA_TEST_GRANTED".into()],
        );

        let out = block(run_tool(&agent, "env_probe", serde_json::json!({}))).unwrap();
        let _ = std::fs::remove_file(&path);

        assert!(out.contains("granted=yes"), "allowlisted secret should pass: {out}");
        assert!(
            out.contains("denied=MISSING"),
            "un-allowlisted var must be withheld: {out}"
        );
    }

    #[test]
    fn destructive_tool_gated_by_agent_optin_but_write_is_free() {
        use crate::core::SideEffect;

        // A destructive tool is refused before execution unless the agent opts in.
        let (mut agent, path) = agent_with_tool("wipe", "echo done\n", vec![]);
        agent.tools[0].side_effect = SideEffect::Destructive;

        let err = block(run_tool(&agent, "wipe", serde_json::json!({}))).unwrap_err();
        assert!(err.to_string().contains("destructive"), "should be blocked: {err}");

        agent.config.allow_destructive_tools = true;
        let out = block(run_tool(&agent, "wipe", serde_json::json!({}))).unwrap();
        assert_eq!(out, "done", "opted-in destructive tool should run");

        // A write tool is never gated, regardless of the opt-in flag.
        let (mut wagent, wpath) = agent_with_tool("notify", "echo sent\n", vec![]);
        wagent.tools[0].side_effect = SideEffect::Write;
        let out = block(run_tool(&wagent, "notify", serde_json::json!({}))).unwrap();
        assert_eq!(out, "sent", "write tool should run without opt-in");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&wpath);
    }

    #[test]
    fn validate_params_enforces_required_and_types() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "count": { "type": "integer" }
            },
            "required": ["text"]
        });

        // Happy path.
        assert!(validate_params("t", &schema, &serde_json::json!({"text": "hi"})).is_ok());
        assert!(validate_params("t", &schema, &serde_json::json!({"text": "hi", "count": 3})).is_ok());

        // Missing required field.
        let err = validate_params("t", &schema, &serde_json::json!({"count": 3})).unwrap_err();
        assert!(err.to_string().contains("missing required parameter 'text'"), "{err}");

        // Wrong type.
        let err = validate_params("t", &schema, &serde_json::json!({"text": 5})).unwrap_err();
        assert!(err.to_string().contains("should be string"), "{err}");

        // integer is lenient about whole numbers but rejects fractions/strings.
        assert!(validate_params("t", &schema, &serde_json::json!({"text": "x", "count": 2})).is_ok());
        assert!(validate_params("t", &schema, &serde_json::json!({"text": "x", "count": "2"})).is_err());
    }

    #[test]
    fn substitute_vars_only_resolves_allowlisted_secrets() {
        std::env::set_var("AGENTA_SUBST_TOKEN", "sekret");
        let allowed = vec!["AGENTA_SUBST_TOKEN".to_string()];

        // Allowlisted + set → substituted.
        assert_eq!(
            substitute_vars("Bearer ${AGENTA_SUBST_TOKEN}", &allowed).unwrap(),
            "Bearer sekret"
        );
        // No placeholders → passthrough.
        assert_eq!(substitute_vars("https://api.example.com", &allowed).unwrap(), "https://api.example.com");

        // Not in the allowlist → refused (even if the env var happens to exist).
        assert!(substitute_vars("${PATH}", &allowed).is_err());
        // Allowlisted but unset → refused rather than sending a literal.
        let missing = vec!["AGENTA_SUBST_UNSET".to_string()];
        assert!(substitute_vars("${AGENTA_SUBST_UNSET}", &missing).is_err());
        // Malformed placeholder → error.
        assert!(substitute_vars("${AGENTA_SUBST_TOKEN", &allowed).is_err());
    }

    #[test]
    fn validate_params_is_permissive_without_a_schema() {
        // Loose schema (no properties/required) accepts anything.
        let loose = serde_json::json!({"type": "object"});
        assert!(validate_params("t", &loose, &serde_json::json!({"anything": true})).is_ok());
        assert!(validate_params("t", &loose, &serde_json::json!({})).is_ok());
        // Extra/undeclared fields are fine even with a strict schema.
        let schema = serde_json::json!({"properties": {"a": {"type": "string"}}});
        assert!(validate_params("t", &schema, &serde_json::json!({"a": "x", "extra": 9})).is_ok());
    }

    #[test]
    fn handler_that_hangs_is_killed_by_timeout() {
        std::env::set_var("AGENTA_TOOL_TIMEOUT_SECS", "1");
        let (agent, path) = agent_with_tool("sleeper", "sleep 30\n", vec![]);

        let started = std::time::Instant::now();
        let result = block(run_tool(&agent, "sleeper", serde_json::json!({})));
        let elapsed = started.elapsed();
        let _ = std::fs::remove_file(&path);
        std::env::remove_var("AGENTA_TOOL_TIMEOUT_SECS");

        assert!(result.is_err(), "hung handler should error");
        assert!(
            result.unwrap_err().to_string().contains("timed out"),
            "error should mention timeout"
        );
        assert!(elapsed.as_secs() < 10, "should give up quickly, took {elapsed:?}");
    }
}
