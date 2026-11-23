use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde::Deserialize;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use agenta::core::{AppConfig, ExecutionStatus};

use super::state::DaemonState;

const MAX_RECENT_EVENTS: usize = 256;
const MAX_THREAD_MESSAGES: usize = 20;
const SLACK_MAX_CHARS: usize = 3500;

pub async fn start_slack_gateway(
    daemon: Arc<DaemonState>,
    config: &AppConfig,
) -> anyhow::Result<()> {
    if config.slack_bots.is_empty() {
        return Ok(());
    }

    let http = reqwest::Client::new();
    for bot in &config.slack_bots {
        let bot_token = resolve_token(&bot.bot_token);
        let app_token = resolve_token(&bot.app_token);
        let label = bot.name.clone().unwrap_or_else(|| "slack-bot".to_string());

        if bot_token.is_empty() || app_token.is_empty() {
            warn!(
                "Slack bot '{}' has empty/unresolved bot_token or app_token — skipping",
                label
            );
            continue;
        }

        info!(
            "Starting Slack Socket Mode gateway for bot '{}' (agent: {})",
            label, bot.default_agent
        );

        let d = daemon.clone();
        let h = http.clone();
        let default_agent = bot.default_agent.clone();
        tokio::spawn(async move {
            run_slack_bot(d, h, bot_token, app_token, default_agent, label).await;
        });
    }

    Ok(())
}

/// Resolve token: if value starts with '$', treat as env var name.
fn resolve_token(token: &str) -> String {
    if let Some(var) = token.strip_prefix('$') {
        std::env::var(var).unwrap_or_default()
    } else {
        token.to_string()
    }
}

/// Bounded FIFO set used to deduplicate Slack events (retries or DM overlap
/// between `app_mention` and `message`).
struct RecentEvents {
    order: VecDeque<String>,
}

impl RecentEvents {
    fn new() -> Self {
        Self {
            order: VecDeque::with_capacity(MAX_RECENT_EVENTS),
        }
    }

    /// Returns `true` if `key` is new and was recorded; `false` if already seen.
    fn record_if_new(&mut self, key: String) -> bool {
        if self.order.iter().any(|k| k == &key) {
            return false;
        }
        if self.order.len() >= MAX_RECENT_EVENTS {
            self.order.pop_front();
        }
        self.order.push_back(key);
        true
    }
}

async fn run_slack_bot(
    daemon: Arc<DaemonState>,
    http: reqwest::Client,
    bot_token: String,
    app_token: String,
    default_agent: String,
    label: String,
) {
    let recent = Arc::new(Mutex::new(RecentEvents::new()));
    let user_cache: Arc<RwLock<HashMap<String, String>>> = Arc::new(RwLock::new(HashMap::new()));

    // Resolve the bot's own user_id once (retrying until reachable).
    let bot_user_id = loop {
        match fetch_bot_user_id(&http, &bot_token).await {
            Ok(uid) => {
                info!("[{}] Slack auth.test succeeded (bot_user_id={})", label, uid);
                break uid;
            }
            Err(e) => {
                warn!(
                    "[{}] Slack auth.test failed (retrying in 10s): {}",
                    label,
                    redact_slack_tokens(&e.to_string())
                );
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        }
    };

    let mut consecutive_errors: u32 = 0;

    loop {
        let wss_url = match open_socket_mode_url(&http, &app_token).await {
            Ok(url) => url,
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors == 1 {
                    warn!(
                        "[{}] Slack apps.connections.open failed (retrying in 10s): {}",
                        label,
                        redact_slack_tokens(&e.to_string())
                    );
                } else {
                    debug!(
                        "[{}] Slack apps.connections.open error #{} (retrying in 10s): {}",
                        label,
                        consecutive_errors,
                        redact_slack_tokens(&e.to_string())
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        let ws_stream = match tokio_tungstenite::connect_async(&wss_url).await {
            Ok((stream, _)) => {
                consecutive_errors = 0;
                info!("[{}] Connected to Slack Socket Mode", label);
                stream
            }
            Err(e) => {
                consecutive_errors += 1;
                warn!(
                    "[{}] Slack WebSocket connect failed (retrying in 10s): {}",
                    label,
                    redact_slack_tokens(&e.to_string())
                );
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        let (mut write, mut read) = ws_stream.split();
        let mut clean_disconnect = false;

        while let Some(msg_res) = read.next().await {
            let msg = match msg_res {
                Ok(m) => m,
                Err(e) => {
                    warn!(
                        "[{}] Slack WebSocket read error: {}",
                        label,
                        redact_slack_tokens(&e.to_string())
                    );
                    break;
                }
            };

            match msg {
                WsMessage::Ping(payload) => {
                    if let Err(e) = write.send(WsMessage::Pong(payload)).await {
                        warn!("[{}] Failed to send Pong: {}", label, e);
                        break;
                    }
                }
                WsMessage::Pong(_) => {}
                WsMessage::Close(_) => {
                    info!("[{}] Slack WebSocket closed by server; reconnecting", label);
                    clean_disconnect = true;
                    break;
                }
                WsMessage::Text(text) => {
                    let envelope: SocketEnvelope = match serde_json::from_str(&text) {
                        Ok(env) => env,
                        Err(e) => {
                            debug!("[{}] Ignoring unparseable Socket Mode frame: {}", label, e);
                            continue;
                        }
                    };

                    // Acknowledge immediately so Slack doesn't retry the event.
                    if let Some(ref env_id) = envelope.envelope_id {
                        let ack = serde_json::json!({ "envelope_id": env_id }).to_string();
                        if let Err(e) = write.send(WsMessage::Text(ack)).await {
                            warn!("[{}] Failed to ack envelope {}: {}", label, env_id, e);
                            break;
                        }
                    }

                    match envelope.envelope_type.as_str() {
                        "hello" => {
                            debug!("[{}] Received Socket Mode hello", label);
                        }
                        "disconnect" => {
                            info!(
                                "[{}] Slack requested disconnect (reason={}); reconnecting",
                                label,
                                envelope.reason.as_deref().unwrap_or("unknown")
                            );
                            clean_disconnect = true;
                            break;
                        }
                        "events_api" => {
                            let Some(payload) = envelope.payload else { continue };
                            let Some(event) = payload.event else { continue };

                            if !should_handle_event(&event, &bot_user_id) {
                                continue;
                            }

                            let dedup_key = format!("{}:{}", event.channel, event.ts);
                            let is_new = recent
                                .lock()
                                .map(|mut r| r.record_if_new(dedup_key))
                                .unwrap_or(true);
                            if !is_new {
                                debug!("[{}] Skipping duplicate event {}:{}", label, event.channel, event.ts);
                                continue;
                            }

                            // Spawn handler so the WebSocket loop stays responsive.
                            let d = daemon.clone();
                            let h = http.clone();
                            let bt = bot_token.clone();
                            let da = default_agent.clone();
                            let lbl = label.clone();
                            let buid = bot_user_id.clone();
                            let cache = user_cache.clone();
                            tokio::spawn(async move {
                                handle_slack_event(d, h, bt, da, lbl, buid, cache, event).await;
                            });
                        }
                        other => {
                            debug!("[{}] Ignoring Socket Mode envelope type '{}'", label, other);
                        }
                    }
                }
                _ => {}
            }
        }

        let delay_secs = if clean_disconnect { 2 } else { 5 };
        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_slack_event(
    daemon: Arc<DaemonState>,
    http: reqwest::Client,
    bot_token: String,
    default_agent: String,
    label: String,
    bot_user_id: String,
    user_cache: Arc<RwLock<HashMap<String, String>>>,
    event: SlackEvent,
) {
    let raw_text = event.text.clone().unwrap_or_default();
    let stripped = strip_bot_mention(&raw_text, &bot_user_id);
    if stripped.trim().is_empty() {
        return;
    }

    let (agent, cleaned_message) = resolve_agent_and_input(&stripped, Some(&default_agent));

    // In channels, reply inside the thread (creating one off the triggering message if top-level).
    // In DMs, stay in-thread only if the user already started a thread.
    let is_dm = event.channel_type.as_deref() == Some("im");
    let reply_thread_ts: Option<String> = if is_dm {
        event.thread_ts.clone()
    } else {
        Some(event.thread_ts.clone().unwrap_or_else(|| event.ts.clone()))
    };

    info!(
        "[{}] channel={} thread={:?} agent={} input={:?}",
        label,
        event.channel,
        reply_thread_ts,
        agent,
        &cleaned_message[..cleaned_message.len().min(80)]
    );

    // Acknowledge with :eyes: while the agent researches and prepares a reply.
    let _ = add_reaction(&http, &bot_token, &event.channel, &event.ts, "eyes").await;

    // If tagged inside an existing thread, fetch recent messages for context.
    let full_input = if let Some(ref parent_ts) = event.thread_ts {
        match fetch_thread_context(
            &http,
            &bot_token,
            &event.channel,
            parent_ts,
            &event.ts,
            &bot_user_id,
            &user_cache,
        )
        .await
        {
            Ok(Some(ctx)) => {
                let sender = match event.user.as_deref() {
                    Some(uid) => resolve_user_name(&http, &bot_token, uid, &user_cache).await,
                    None => "User".to_string(),
                };
                format!(
                    "[Slack thread context]\n{}\n\n[Latest message from {}]\n{}",
                    ctx, sender, cleaned_message
                )
            }
            Ok(None) => cleaned_message,
            Err(e) => {
                debug!(
                    "[{}] Could not fetch thread context for {}:{}: {}",
                    label,
                    event.channel,
                    parent_ts,
                    redact_slack_tokens(&e.to_string())
                );
                cleaned_message
            }
        }
    } else {
        cleaned_message
    };

    let reply = match daemon.run_agent_sync_execution(&agent, full_input).await {
        Ok(execution) => {
            if execution.status == ExecutionStatus::Cancelled {
                let _ = remove_reaction(&http, &bot_token, &event.channel, &event.ts, "eyes").await;
                return;
            }
            let raw = execution.output.as_deref().unwrap_or("").to_string();
            let trimmed_raw = raw.trim_start();
            if raw.trim().is_empty()
                || trimmed_raw.starts_with("TOOL_CALL:")
                || trimmed_raw.starts_with("<tool_calls")
                || trimmed_raw.starts_with("<invoke ")
            {
                "Sorry, I couldn't generate a response. Please try again.".to_string()
            } else {
                markdown_to_slack_mrkdwn(&raw)
            }
        }
        Err(e) => format!("Agent error: {}", e),
    };

    if let Err(e) = post_slack_message(
        &http,
        &bot_token,
        &event.channel,
        reply_thread_ts.as_deref(),
        &reply,
    )
    .await
    {
        warn!(
            "[{}] Failed posting Slack reply to {}: {}",
            label,
            event.channel,
            redact_slack_tokens(&e.to_string())
        );
    }

    let _ = remove_reaction(&http, &bot_token, &event.channel, &event.ts, "eyes").await;
}

// ── Slack Socket Mode & Web API structs ───────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SocketEnvelope {
    envelope_id: Option<String>,
    #[serde(rename = "type")]
    envelope_type: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    payload: Option<EventsApiPayload>,
}

#[derive(Debug, Deserialize)]
struct EventsApiPayload {
    #[serde(default)]
    event: Option<SlackEvent>,
}

#[derive(Debug, Clone, Deserialize)]
struct SlackEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    channel: String,
    #[serde(default)]
    channel_type: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    bot_id: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    ts: String,
    #[serde(default)]
    thread_ts: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackApiOk {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthTestResponse {
    ok: bool,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConnectionsOpenResponse {
    ok: bool,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConversationsRepliesResponse {
    ok: bool,
    #[serde(default)]
    messages: Vec<ThreadMessage>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ThreadMessage {
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    bot_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    ts: String,
}

#[derive(Debug, Deserialize)]
struct UsersInfoResponse {
    ok: bool,
    #[serde(default)]
    user: Option<SlackUserObj>,
}

#[derive(Debug, Deserialize)]
struct SlackUserObj {
    #[serde(default)]
    real_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    profile: Option<SlackUserProfile>,
}

#[derive(Debug, Deserialize)]
struct SlackUserProfile {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    real_name: Option<String>,
}

// ── Event filtering & formatting helpers ──────────────────────────────────────

/// Decide whether a Slack Events API event should trigger an agent run:
/// - `app_mention` in channels (or threads)
/// - `message` in direct messages (`channel_type == "im"`)
/// Always ignores bot messages, message edits/subtypes, and the bot's own messages.
fn should_handle_event(event: &SlackEvent, bot_user_id: &str) -> bool {
    if event.channel.is_empty() || event.ts.is_empty() {
        return false;
    }
    if event.bot_id.is_some() || event.subtype.is_some() {
        return false;
    }
    if !bot_user_id.is_empty() && event.user.as_deref() == Some(bot_user_id) {
        return false;
    }

    match event.event_type.as_str() {
        "app_mention" => true,
        "message" => event.channel_type.as_deref() == Some("im"),
        _ => false,
    }
}

/// Remove `<@BOT_USER_ID>` tags from the message text so the agent sees the real question.
fn strip_bot_mention(text: &str, bot_user_id: &str) -> String {
    if bot_user_id.is_empty() {
        return text.trim().to_string();
    }
    let tag = format!("<@{}>", bot_user_id);
    text.replace(&tag, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolve_agent_and_input(text: &str, default_agent: Option<&str>) -> (String, String) {
    if let Some(rest) = text.strip_prefix("/agent ") {
        let mut parts = rest.splitn(2, ' ');
        if let (Some(agent), Some(message)) = (parts.next(), parts.next()) {
            return (agent.to_string(), message.to_string());
        }
    }
    let agent = default_agent.unwrap_or("MIND").to_string();
    (agent, text.to_string())
}

/// Convert standard Markdown into Slack `mrkdwn`, leaving fenced code blocks untouched.
fn markdown_to_slack_mrkdwn(input: &str) -> String {
    let normalized = input.replace('\r', "");
    let mut out_parts: Vec<String> = Vec::new();
    let mut in_code_block = false;
    let mut current_chunk = String::new();

    for line in normalized.split('\n') {
        if line.trim_start().starts_with("```") {
            if in_code_block {
                current_chunk.push_str(line);
                out_parts.push(std::mem::take(&mut current_chunk));
                in_code_block = false;
            } else {
                if !current_chunk.is_empty() {
                    out_parts.push(convert_non_code_markdown(&std::mem::take(&mut current_chunk)));
                }
                current_chunk.push_str(line);
                current_chunk.push('\n');
                in_code_block = true;
            }
        } else {
            current_chunk.push_str(line);
            current_chunk.push('\n');
        }
    }

    if !current_chunk.is_empty() {
        if in_code_block {
            out_parts.push(current_chunk);
        } else {
            out_parts.push(convert_non_code_markdown(&current_chunk));
        }
    }

    out_parts.join("\n").trim().to_string()
}

fn convert_non_code_markdown(text: &str) -> String {
    let mut s = text.to_string();

    // [label](url) -> <url|label>
    if let Ok(re_link) = Regex::new(r"\[([^\]]+)\]\((https?://[^)\s]+)\)") {
        s = re_link.replace_all(&s, "<$2|$1>").into_owned();
    }
    // **bold** and __bold__ -> *bold* (avoid mangling snake_case / mcp__server__tool)
    if let Ok(re_bold_star) = Regex::new(r"\*\*(.+?)\*\*") {
        s = re_bold_star.replace_all(&s, "*$1*").into_owned();
    }
    if let Ok(re_bold_under) = Regex::new(r"(^|[^\w])__([^_\n]+?)__([^\w]|$)") {
        s = re_bold_under.replace_all(&s, "$1*$2*$3").into_owned();
    }
    // ~~strike~~ -> ~strike~
    if let Ok(re_strike) = Regex::new(r"~~(.+?)~~") {
        s = re_strike.replace_all(&s, "~$1~").into_owned();
    }

    // Headings (# / ## / ###) -> *Heading*, and collapse excessive blank lines.
    let mut cleaned_lines = Vec::new();
    let mut empty_streak = 0usize;
    for line in s.lines() {
        let trimmed_end = line.trim_end();
        let converted = if let Some(rest) = trimmed_end.strip_prefix("### ") {
            format!("*{}*", rest.trim())
        } else if let Some(rest) = trimmed_end.strip_prefix("## ") {
            format!("*{}*", rest.trim())
        } else if let Some(rest) = trimmed_end.strip_prefix("# ") {
            format!("*{}*", rest.trim())
        } else {
            trimmed_end.to_string()
        };

        if converted.is_empty() {
            empty_streak += 1;
            if empty_streak <= 1 {
                cleaned_lines.push(String::new());
            }
            continue;
        }
        empty_streak = 0;
        cleaned_lines.push(converted);
    }

    cleaned_lines.join("\n")
}

/// Split long replies into Slack-safe chunks, preferring newline boundaries.
fn split_for_slack(input: &str, max_chars: usize) -> Vec<String> {
    if input.chars().count() <= max_chars {
        return vec![input.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for line in input.split('\n') {
        let line_len = line.chars().count();
        if current_len + line_len + 1 > max_chars && !current.is_empty() {
            chunks.push(current);
            current = String::new();
            current_len = 0;
        }
        if line_len > max_chars {
            // Hard-split a single overly long line.
            for ch in line.chars() {
                if current_len >= max_chars {
                    chunks.push(current);
                    current = String::new();
                    current_len = 0;
                }
                current.push(ch);
                current_len += 1;
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
                current_len += 1;
            }
            current.push_str(line);
            current_len += line_len;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Scrub Slack tokens (`xoxb-...`, `xapp-...`, `xoxp-...`) from error strings before logging.
fn redact_slack_tokens(s: &str) -> String {
    match Regex::new(r"x(?:ox[a-z]|app)-[A-Za-z0-9-]+") {
        Ok(re) => re.replace_all(s, "x***-<redacted>").into_owned(),
        Err(_) => s.to_string(),
    }
}

// ── Slack Web API calls ───────────────────────────────────────────────────────

async fn fetch_bot_user_id(http: &reqwest::Client, bot_token: &str) -> anyhow::Result<String> {
    let resp = http
        .post("https://slack.com/api/auth.test")
        .bearer_auth(bot_token)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?;

    let data: AuthTestResponse = resp.json().await?;
    if !data.ok {
        anyhow::bail!(
            "auth.test returned ok=false: {}",
            data.error.unwrap_or_else(|| "unknown".into())
        );
    }
    data.user_id
        .ok_or_else(|| anyhow::anyhow!("auth.test missing user_id"))
}

async fn open_socket_mode_url(http: &reqwest::Client, app_token: &str) -> anyhow::Result<String> {
    let resp = http
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(app_token)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?;

    let data: ConnectionsOpenResponse = resp.json().await?;
    if !data.ok {
        anyhow::bail!(
            "apps.connections.open failed: {}",
            data.error.unwrap_or_else(|| "unknown".into())
        );
    }
    data.url
        .ok_or_else(|| anyhow::anyhow!("apps.connections.open missing url"))
}

async fn post_slack_message(
    http: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    thread_ts: Option<&str>,
    text: &str,
) -> anyhow::Result<()> {
    for chunk in split_for_slack(text, SLACK_MAX_CHARS) {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": chunk,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let resp = http
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(bot_token)
            .json(&body)
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await?;

        let data: SlackApiOk = resp.json().await?;
        if !data.ok {
            anyhow::bail!(
                "chat.postMessage failed: {}",
                data.error.unwrap_or_else(|| "unknown".into())
            );
        }
    }
    Ok(())
}

async fn add_reaction(
    http: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    timestamp: &str,
    emoji: &str,
) -> anyhow::Result<()> {
    let resp = http
        .post("https://slack.com/api/reactions.add")
        .bearer_auth(bot_token)
        .json(&serde_json::json!({
            "channel": channel,
            "timestamp": timestamp,
            "name": emoji,
        }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    let data: SlackApiOk = resp.json().await?;
    if !data.ok {
        debug!(
            "Slack reactions.add ({}) ignored error: {}",
            emoji,
            data.error.unwrap_or_else(|| "unknown".into())
        );
    }
    Ok(())
}

async fn remove_reaction(
    http: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    timestamp: &str,
    emoji: &str,
) -> anyhow::Result<()> {
    let resp = http
        .post("https://slack.com/api/reactions.remove")
        .bearer_auth(bot_token)
        .json(&serde_json::json!({
            "channel": channel,
            "timestamp": timestamp,
            "name": emoji,
        }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    let data: SlackApiOk = resp.json().await?;
    if !data.ok {
        debug!(
            "Slack reactions.remove ({}) ignored error: {}",
            emoji,
            data.error.unwrap_or_else(|| "unknown".into())
        );
    }
    Ok(())
}

async fn fetch_thread_context(
    http: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    thread_ts: &str,
    current_ts: &str,
    bot_user_id: &str,
    user_cache: &Arc<RwLock<HashMap<String, String>>>,
) -> anyhow::Result<Option<String>> {
    let resp = http
        .get("https://slack.com/api/conversations.replies")
        .bearer_auth(bot_token)
        .query(&[
            ("channel", channel),
            ("ts", thread_ts),
            ("limit", "30"),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?;

    let data: ConversationsRepliesResponse = resp.json().await?;
    if !data.ok {
        anyhow::bail!(
            "conversations.replies failed: {}",
            data.error.unwrap_or_else(|| "unknown".into())
        );
    }

    let prior: Vec<&ThreadMessage> = data
        .messages
        .iter()
        .filter(|m| m.ts != current_ts && m.text.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false))
        .collect();

    if prior.is_empty() {
        return Ok(None);
    }

    let start = prior.len().saturating_sub(MAX_THREAD_MESSAGES);
    let mut lines = Vec::new();
    for m in &prior[start..] {
        let speaker = if m.user.as_deref() == Some(bot_user_id) || m.bot_id.is_some() {
            "Assistant".to_string()
        } else if let Some(uid) = m.user.as_deref() {
            resolve_user_name(http, bot_token, uid, user_cache).await
        } else {
            "User".to_string()
        };
        let body = strip_bot_mention(m.text.as_deref().unwrap_or(""), bot_user_id);
        if !body.is_empty() {
            lines.push(format!("{}: {}", speaker, body));
        }
    }

    if lines.is_empty() {
        Ok(None)
    } else {
        Ok(Some(lines.join("\n")))
    }
}

async fn resolve_user_name(
    http: &reqwest::Client,
    bot_token: &str,
    user_id: &str,
    cache: &Arc<RwLock<HashMap<String, String>>>,
) -> String {
    if let Some(cached) = cache.read().await.get(user_id) {
        return cached.clone();
    }

    let name = match http
        .get("https://slack.com/api/users.info")
        .bearer_auth(bot_token)
        .query(&[("user", user_id)])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => resp
            .json::<UsersInfoResponse>()
            .await
            .ok()
            .filter(|r| r.ok)
            .and_then(|r| r.user)
            .and_then(|u| {
                u.profile
                    .as_ref()
                    .and_then(|p| p.display_name.as_deref().filter(|s| !s.is_empty()).map(str::to_string))
                    .or_else(|| {
                        u.profile
                            .as_ref()
                            .and_then(|p| p.real_name.as_deref().filter(|s| !s.is_empty()).map(str::to_string))
                    })
                    .or_else(|| u.real_name.filter(|s| !s.is_empty()))
                    .or_else(|| u.name.filter(|s| !s.is_empty()))
            })
            .unwrap_or_else(|| user_id.to_string()),
        Err(_) => user_id.to_string(),
    };

    cache.write().await.insert(user_id.to_string(), name.clone());
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(
        event_type: &str,
        channel_type: Option<&str>,
        user: Option<&str>,
        bot_id: Option<&str>,
        subtype: Option<&str>,
    ) -> SlackEvent {
        SlackEvent {
            event_type: event_type.to_string(),
            channel: "C12345".to_string(),
            channel_type: channel_type.map(str::to_string),
            user: user.map(str::to_string),
            bot_id: bot_id.map(str::to_string),
            subtype: subtype.map(str::to_string),
            text: Some("<@U_BOT> hello".to_string()),
            ts: "1700000000.000100".to_string(),
            thread_ts: None,
        }
    }

    #[test]
    fn event_filter_accepts_app_mention_and_dm_only() {
        assert!(should_handle_event(
            &sample_event("app_mention", Some("channel"), Some("U_ALICE"), None, None),
            "U_BOT"
        ));
        assert!(should_handle_event(
            &sample_event("message", Some("im"), Some("U_ALICE"), None, None),
            "U_BOT"
        ));
        // Normal channel message without mention → ignored
        assert!(!should_handle_event(
            &sample_event("message", Some("channel"), Some("U_ALICE"), None, None),
            "U_BOT"
        ));
        // Bot's own message or other bot → ignored
        assert!(!should_handle_event(
            &sample_event("message", Some("im"), Some("U_BOT"), None, None),
            "U_BOT"
        ));
        assert!(!should_handle_event(
            &sample_event("message", Some("im"), Some("U_OTHER"), Some("B999"), None),
            "U_BOT"
        ));
        // Message edits / subtypes → ignored
        assert!(!should_handle_event(
            &sample_event("message", Some("im"), Some("U_ALICE"), None, Some("message_changed")),
            "U_BOT"
        ));
    }

    #[test]
    fn strip_bot_mention_removes_tag_cleanly() {
        assert_eq!(strip_bot_mention("<@U123BOT> what broke in prod?", "U123BOT"), "what broke in prod?");
        assert_eq!(strip_bot_mention("hey <@U123BOT> check PROJ-42", "U123BOT"), "hey check PROJ-42");
        assert_eq!(strip_bot_mention("no tag here", "U123BOT"), "no tag here");
    }

    #[test]
    fn recent_events_deduplicates_and_evicts() {
        let mut recent = RecentEvents::new();
        assert!(recent.record_if_new("C1:1.0".into()));
        assert!(!recent.record_if_new("C1:1.0".into()));
        assert!(recent.record_if_new("C1:2.0".into()));
    }

    #[test]
    fn markdown_converts_to_slack_mrkdwn_and_preserves_code_blocks() {
        let md = "## Status\nCheck **PROJ-123** in [Jira](https://jira.example.com/PROJ-123) and ~~old~~.\n```rust\nlet x = **not_bold**;\n```";
        let out = markdown_to_slack_mrkdwn(md);
        assert!(out.contains("*Status*"));
        assert!(out.contains("*PROJ-123*"));
        assert!(out.contains("<https://jira.example.com/PROJ-123|Jira>"));
        assert!(out.contains("~old~"));
        assert!(out.contains("let x = **not_bold**;"));
    }

    #[test]
    fn redact_slack_tokens_scrubs_all_token_types() {
        let err = "failed auth with xoxb-123456-7890-abcdef and xapp-1-A0123-4567-xyz";
        let out = redact_slack_tokens(err);
        assert!(!out.contains("xoxb-123456"));
        assert!(!out.contains("xapp-1-A0123"));
        assert!(out.contains("x***-<redacted>"));
    }

    #[test]
    fn split_for_slack_respects_limit() {
        let text = format!("{}\n{}", "a".repeat(20), "b".repeat(20));
        let chunks = split_for_slack(&text, 25);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "a".repeat(20));
        assert_eq!(chunks[1], "b".repeat(20));
    }
}
