use std::sync::Arc;

use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use agenta::core::{AppConfig, ExecutionStatus, TelegramBotConfig};

use super::state::DaemonState;

pub async fn start_chat_gateway(
    daemon: Arc<DaemonState>,
    config: &AppConfig,
) -> anyhow::Result<()> {
    // ── Build effective bot list (multi-bot + legacy single-bot) ─────────────
    let mut bots: Vec<TelegramBotConfig> = config.telegram_bots.clone();

    // Backward-compat: synthesise from legacy telegram_bot_token field
    if let Some(token) = &config.telegram_bot_token {
        let already_present = bots.iter().any(|b| resolve_token(&b.token) == *token);
        if !already_present {
            bots.push(TelegramBotConfig {
                token: token.clone(),
                default_agent: config
                    .telegram_default_agent
                    .clone()
                    .unwrap_or_else(|| "travel-guide".to_string()),
                name: Some("legacy".to_string()),
            });
        }
    }

    if bots.is_empty() {
        return Ok(());
    }

    // ── Start one long-polling loop per bot ───────────────────────────────────
    let http = reqwest::Client::new();
    for bot in bots {
        let token = resolve_token(&bot.token);
        if token.is_empty() {
            warn!(
                "Telegram bot '{}' has empty/unresolved token — skipping",
                bot.name.as_deref().unwrap_or("unnamed")
            );
            continue;
        }
        let label = bot.name.clone().unwrap_or_else(|| "bot".to_string());
        info!(
            "Starting Telegram polling for bot '{}' (agent: {})",
            label, bot.default_agent
        );
        let d = daemon.clone();
        let h = http.clone();
        tokio::spawn(async move {
            poll_telegram_bot(d, h, token, bot.default_agent, label).await;
        });
    }

    Ok(())
}

// ── Long-polling loop ─────────────────────────────────────────────────────────

/// Resolve token: if value starts with '$', treat as env var name.
fn resolve_token(token: &str) -> String {
    if let Some(var) = token.strip_prefix('$') {
        std::env::var(var).unwrap_or_default()
    } else {
        token.to_string()
    }
}

async fn poll_telegram_bot(
    daemon: Arc<DaemonState>,
    http: reqwest::Client,
    token: String,
    default_agent: String,
    label: String,
) {
    let base_url = format!("https://api.telegram.org/bot{}", token);
    let mut offset: i64 = 0;

    loop {
        let updates = match fetch_updates(&http, &base_url, offset).await {
            Ok(u) => u,
            Err(e) => {
                warn!("[{}] getUpdates error: {} — retrying in 10s", label, e);
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        };

        for update in updates {
            offset = offset.max(update.update_id + 1);

            let (chat_id, text) = match update.message {
                Some(msg) => (msg.chat.id, msg.text.unwrap_or_default()),
                None => continue,
            };

            if text.trim().is_empty() {
                continue;
            }

            let (agent, input) = resolve_agent_and_input(&text, Some(&default_agent));

            info!(
                "[{}] chat={} agent={} input={:?}",
                label,
                chat_id,
                agent,
                &input[..input.len().min(80)]
            );

            // Process in background so the poll loop stays responsive
            let d = daemon.clone();
            let h = http.clone();
            let bu = base_url.clone();
            let lbl = label.clone();
            tokio::spawn(async move {
                // Set up progress channel — sub-agent notifications fire through this
                let (progress_tx, mut progress_rx) =
                    tokio::sync::mpsc::unbounded_channel::<String>();

                // Forward progress messages to Telegram as they arrive
                let h_prog = h.clone();
                let bu_prog = bu.clone();
                let lbl_prog = lbl.clone();
                tokio::spawn(async move {
                    while let Some(msg) = progress_rx.recv().await {
                        if let Err(e) =
                            send_telegram_message(&h_prog, &bu_prog, chat_id, &msg).await
                        {
                            warn!(
                                "[{}] Failed sending progress to chat {}: {}",
                                lbl_prog, chat_id, e
                            );
                        }
                    }
                });

                // Show "typing..." indicator while agent is processing
                let typing_cancel = CancellationToken::new();
                let h_typing = h.clone();
                let bu_typing = bu.clone();
                let lbl_typing = lbl.clone();
                let cancel_clone = typing_cancel.clone();
                tokio::spawn(async move {
                    loop {
                        let _ = h_typing
                            .post(&format!("{}/sendChatAction", bu_typing))
                            .json(&serde_json::json!({
                                "chat_id": chat_id,
                                "action": "typing"
                            }))
                            .send()
                            .await;
                        tokio::select! {
                            _ = cancel_clone.cancelled() => break,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {}
                        }
                    }
                    info!("[{}] Typing indicator stopped for chat {}", lbl_typing, chat_id);
                });

                let reply = match d
                    .run_agent_sync_execution_with_progress(&agent, input, progress_tx)
                    .await
                {
                    Ok(execution) => {
                        // Skip silently if execution was cancelled
                        if execution.status == ExecutionStatus::Cancelled {
                            typing_cancel.cancel();
                            return;
                        }
                        let raw = execution.output.as_deref().unwrap_or("").to_string();
                        if raw.trim().is_empty() || raw.trim_start().starts_with("TOOL_CALL:") {
                            "Sorry, I couldn't generate a response. Please try again.".to_string()
                        } else {
                            sanitize_for_chat(&raw)
                        }
                    }
                    Err(e) => format!("Agent error: {}", e),
                };

                // Stop typing indicator before sending reply
                typing_cancel.cancel();

                if let Err(e) = send_telegram_message(&h, &bu, chat_id, &reply).await {
                    warn!("[{}] Failed sending reply to chat {}: {}", lbl, chat_id, e);
                }
            });
        }
    }
}

#[derive(Debug, Deserialize)]
struct TelegramGetUpdatesResponse {
    ok: bool,
    result: Vec<TelegramUpdate>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

async fn fetch_updates(
    http: &reqwest::Client,
    base_url: &str,
    offset: i64,
) -> anyhow::Result<Vec<TelegramUpdate>> {
    let url = format!("{}/getUpdates", base_url);
    let resp = http
        .get(&url)
        .query(&[
            ("offset", offset.to_string()),
            ("timeout", "30".to_string()),
            ("limit", "100".to_string()),
            ("allowed_updates", r#"["message"]"#.to_string()),
        ])
        .timeout(std::time::Duration::from_secs(40))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("getUpdates failed: {} {}", status, body);
    }

    let data: TelegramGetUpdatesResponse = resp.json().await?;
    if !data.ok {
        anyhow::bail!("Telegram API returned ok=false");
    }
    Ok(data.result)
}

async fn send_telegram_message(
    http: &reqwest::Client,
    base_url: &str,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!("{}/sendMessage", base_url);
    for chunk in split_for_telegram(text, 3500) {
        let response = http
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": chunk
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Telegram sendMessage failed: {} {}", status, body);
        }
    }
    Ok(())
}

fn split_for_telegram(input: &str, max_chars: usize) -> Vec<String> {
    if input.is_empty() {
        return vec![String::new()];
    }
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut count = 0usize;
    for ch in input.chars() {
        if count >= max_chars {
            parts.push(buf);
            buf = String::new();
            count = 0;
        }
        buf.push(ch);
        count += 1;
    }
    if !buf.is_empty() {
        parts.push(buf);
    }
    parts
}

fn resolve_agent_and_input(text: &str, default_agent: Option<&str>) -> (String, String) {
    // "/agent <name> <message>" overrides default agent
    if let Some(rest) = text.strip_prefix("/agent ") {
        let mut parts = rest.splitn(2, ' ');
        if let (Some(agent), Some(message)) = (parts.next(), parts.next()) {
            return (agent.to_string(), message.to_string());
        }
    }
    let agent = default_agent.unwrap_or("travel-guide").to_string();
    (agent, text.to_string())
}

fn sanitize_for_chat(input: &str) -> String {
    let mut out = input.replace('\r', "");
    out = out.replace("**", "");
    out = out.replace("__", "");
    out = out.replace("### ", "");
    out = out.replace("## ", "");
    out = out.replace("# ", "");
    out = out.replace("<br>", "\n");
    out = out.replace("<br/>", "\n");
    out = out.replace("<br />", "\n");

    let mut cleaned_lines = Vec::new();
    let mut empty_streak = 0usize;
    for line in out.lines() {
        let trimmed = line.trim_end();
        let is_separator =
            trimmed.chars().all(|c| c == '-' || c == '—') && trimmed.len() >= 3;
        if is_separator {
            continue;
        }
        if trimmed.is_empty() {
            empty_streak += 1;
            if empty_streak <= 1 {
                cleaned_lines.push(String::new());
            }
            continue;
        }
        empty_streak = 0;
        cleaned_lines.push(trimmed.to_string());
    }

    cleaned_lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_for_chat;

    #[test]
    fn sanitize_for_chat_removes_markdown_noise() {
        let input = "## Title\n**bold** text\n---\nline";
        let out = sanitize_for_chat(input);
        assert!(!out.contains("##"));
        assert!(!out.contains("**"));
        assert!(!out.contains("---"));
        assert!(out.contains("Title"));
        assert!(out.contains("bold text"));
    }

    #[test]
    fn sanitize_for_chat_collapses_excess_blank_lines() {
        let input = "line1\n\n\n\nline2";
        let out = sanitize_for_chat(input);
        assert_eq!(out, "line1\n\nline2");
    }

    #[test]
    fn resolve_token_reads_env_var() {
        std::env::set_var("TEST_BOT_TOKEN", "abc123");
        assert_eq!(super::resolve_token("$TEST_BOT_TOKEN"), "abc123");
        assert_eq!(super::resolve_token("literal"), "literal");
    }
}
