use std::sync::Arc;

use axum::{
    extract::{Form, State},
    http::{header::CONTENT_TYPE, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use tracing::{error, info, warn};

use agenta::core::AppConfig;

use super::state::DaemonState;

#[derive(Clone)]
struct ChatState {
    daemon: Arc<DaemonState>,
    telegram_bot_token: Option<String>,
    telegram_default_agent: Option<String>,
    whatsapp_default_agent: Option<String>,
    http: reqwest::Client,
}

pub async fn start_chat_gateway(
    daemon: Arc<DaemonState>,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let enabled = config.telegram_bot_token.is_some() || config.whatsapp_default_agent.is_some();
    if !enabled {
        return Ok(());
    }

    let state = ChatState {
        daemon,
        telegram_bot_token: config.telegram_bot_token.clone(),
        telegram_default_agent: config.telegram_default_agent.clone(),
        whatsapp_default_agent: config.whatsapp_default_agent.clone(),
        http: reqwest::Client::new(),
    };

    let app = Router::new()
        .route("/telegram/webhook", post(telegram_webhook))
        .route("/whatsapp/webhook", post(whatsapp_webhook))
        .with_state(state);

    let addr: std::net::SocketAddr = format!("127.0.0.1:{}", config.chat_gateway_port)
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid chat gateway addr: {}", e))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    info!(
        "Chat gateway listening on http://127.0.0.1:{}",
        config.chat_gateway_port
    );

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            error!("Chat gateway failed: {}", e);
        }
    });

    Ok(())
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
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

async fn telegram_webhook(
    State(state): State<ChatState>,
    Json(update): Json<TelegramUpdate>,
) -> impl IntoResponse {
    let Some(msg) = update.message else {
        return (StatusCode::OK, "ignored").into_response();
    };
    let Some(text) = msg.text else {
        return (StatusCode::OK, "ignored").into_response();
    };

    let (agent, input) = resolve_agent_and_input(&text, state.telegram_default_agent.as_deref());
    let execution = match state.daemon.run_agent_sync_execution(&agent, input).await {
        Ok(execution) => execution,
        Err(e) => {
            let reply = format!("Agent error: {}", e);
            if let Some(token) = &state.telegram_bot_token {
                if let Err(send_err) = send_telegram_message(&state.http, token, msg.chat.id, &reply).await {
                    warn!("Failed sending Telegram reply: {}", send_err);
                }
            }
            return (StatusCode::OK, "ok").into_response();
        }
    };

    let reply = execution
        .output
        .as_deref()
        .map(sanitize_for_chat)
        .unwrap_or_default();

    if let Some(token) = &state.telegram_bot_token {
        let text_reply = if reply.trim().is_empty() || reply.trim_start().starts_with("TOOL_CALL:") {
            "Sorry, I couldn't format that response. Please try again."
                .to_string()
        } else {
            reply
        };
        if let Err(e) = send_telegram_message(&state.http, token, msg.chat.id, &text_reply).await {
            warn!("Failed sending Telegram reply: {}", e);
        }
    }

    (StatusCode::OK, "ok").into_response()
}

async fn send_telegram_message(
    http: &reqwest::Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> anyhow::Result<()> {
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    // Telegram sendMessage has a hard message length limit (~4096 chars).
    // Split long model outputs to avoid silent delivery failures.
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
            anyhow::bail!("Telegram send failed: {} {}", status, body);
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

async fn whatsapp_webhook(
    State(state): State<ChatState>,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let body = form.get("Body").cloned().unwrap_or_default();
    if body.trim().is_empty() {
        return twiml_response("Empty message");
    }

    let (agent, input) = resolve_agent_and_input(&body, state.whatsapp_default_agent.as_deref());
    let reply = match state.daemon.run_agent_sync(&agent, input).await {
        Ok(output) => output,
        Err(e) => format!("Agent error: {}", e),
    };
    twiml_response(&sanitize_for_chat(&reply))
}

fn twiml_response(msg: &str) -> (StatusCode, HeaderMap, String) {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, "application/xml".parse().unwrap());
    let escaped = msg
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(r#"<?xml version="1.0" encoding="UTF-8"?><Response><Message>{}</Message></Response>"#, escaped);
    (StatusCode::OK, headers, body)
}

fn resolve_agent_and_input(text: &str, default_agent: Option<&str>) -> (String, String) {
    // "/agent <name> <message>" overrides default agent.
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
    out = out.replace("```", "");
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
        let is_separator = trimmed.chars().all(|c| c == '-' || c == '—') && trimmed.len() >= 3;
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
}
