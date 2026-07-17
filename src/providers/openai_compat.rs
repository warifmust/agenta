use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::core::{AgentaError, Result};
use crate::ollama::client::{
    ChatMessage, ChatRequest, ChatResponse, GenerateRequest, GenerateResponse,
};
use super::ModelBackend;

// ── OpenAI-compatible request/response types ──────────────────────────────────

#[derive(Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<i64>,
    stream: bool,
}

#[derive(Serialize, Deserialize)]
struct OpenAIMessage {
    role: String,
    // Responses can carry `content: null` (reasoning/MoE models like DeepSeek
    // return null when emitting reasoning or tool calls). Treat null/missing as
    // empty so deserialization never fails. Requests always set a real string.
    #[serde(default, deserialize_with = "null_to_empty_string")]
    content: String,
}

fn null_to_empty_string<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
    #[serde(default)]
    usage: Option<OpenAIUsage>,
}

#[derive(Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
}

#[derive(Deserialize)]
struct OpenAIUsage {
    #[serde(default)]
    total_tokens: i64,
    #[serde(default)]
    prompt_tokens: i64,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct OpenAICompatClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl OpenAICompatClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default();
        Self { client, base_url, api_key }
    }

    /// Extract temperature/top_p/max_tokens from Ollama-style options blob.
    fn extract_params(options: &Option<serde_json::Value>) -> (Option<f32>, Option<f32>, Option<i64>) {
        let Some(opts) = options else {
            return (None, None, None);
        };
        let temperature = opts.get("temperature").and_then(|v| v.as_f64()).map(|v| v as f32);
        let top_p = opts.get("top_p").and_then(|v| v.as_f64()).map(|v| v as f32);
        // Ollama uses num_predict for max tokens; OpenAI uses max_tokens
        let max_tokens = opts
            .get("num_predict")
            .and_then(|v| v.as_i64())
            .filter(|&n| n > 0); // -1 means unlimited in Ollama — omit for OpenAI
        (temperature, top_p, max_tokens)
    }

    /// Returns (content, total_tokens, prompt_tokens) from the provider's `usage`
    /// block (each None if it didn't report one). prompt_tokens is the input/context
    /// side, used for the context-fullness meter.
    async fn call(&self, req: OpenAIRequest) -> Result<(String, Option<u64>, Option<u64>)> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AgentaError::Ollama(format!("HTTP {}: {}", status, text)));
        }

        let parsed: OpenAIResponse = response.json().await.map_err(|e| {
            AgentaError::Ollama(format!("Failed to parse response: {}", e))
        })?;

        let tokens = parsed.usage.as_ref().map(|u| u.total_tokens.max(0) as u64);
        let prompt = parsed.usage.as_ref().map(|u| u.prompt_tokens.max(0) as u64);
        let content = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        Ok((content, tokens, prompt))
    }
}

#[async_trait]
impl ModelBackend for OpenAICompatClient {
    /// generate() — used for single-turn prompts (no chat history).
    /// Maps to a two-message chat request: system + user.
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse> {
        let (temperature, top_p, max_tokens) = Self::extract_params(&request.options);

        let mut messages = Vec::new();
        if let Some(system) = &request.system {
            messages.push(OpenAIMessage { role: "system".to_string(), content: system.clone() });
        }
        messages.push(OpenAIMessage { role: "user".to_string(), content: request.prompt.clone() });

        let openai_req = OpenAIRequest {
            model: request.model.clone(),
            messages,
            temperature,
            top_p,
            max_tokens,
            stream: false,
        };

        let (content, tokens, _prompt) = self.call(openai_req).await?;

        Ok(GenerateResponse {
            model: request.model,
            created_at: String::new(),
            response: content,
            done: true,
            context: None,
            total_duration: None,
            load_duration: None,
            prompt_eval_count: None,
            prompt_eval_duration: None,
            eval_count: tokens.map(|t| t as i64),
            eval_duration: None,
        })
    }

    /// chat() — used for multi-turn conversations.
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let (temperature, top_p, max_tokens) = Self::extract_params(&request.options);

        let messages: Vec<OpenAIMessage> = request
            .messages
            .iter()
            .map(|m| OpenAIMessage { role: m.role.clone(), content: m.content.clone() })
            .collect();

        let openai_req = OpenAIRequest {
            model: request.model.clone(),
            messages,
            temperature,
            top_p,
            max_tokens,
            stream: false,
        };

        let (content, total_tokens, prompt_tokens) = self.call(openai_req).await?;

        Ok(ChatResponse {
            model: request.model,
            created_at: String::new(),
            message: ChatMessage { role: "assistant".to_string(), content },
            done: true,
            total_tokens,
            prompt_tokens,
            prompt_eval_count: None,
            eval_count: None,
        })
    }
}
