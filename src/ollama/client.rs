use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::core::{AgentaError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub model: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateResponse {
    pub model: String,
    pub created_at: String,
    pub response: String,
    pub done: bool,
    #[serde(default)]
    pub context: Option<Vec<i64>>,
    #[serde(default)]
    pub total_duration: Option<i64>,
    #[serde(default)]
    pub load_duration: Option<i64>,
    #[serde(default)]
    pub prompt_eval_count: Option<i64>,
    #[serde(default)]
    pub prompt_eval_duration: Option<i64>,
    #[serde(default)]
    pub eval_count: Option<i64>,
    #[serde(default)]
    pub eval_duration: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub model: String,
    pub created_at: String,
    pub message: ChatMessage,
    pub done: bool,
    // Token usage. OpenAI-compatible providers report `total_tokens`; Ollama
    // returns prompt_eval_count + eval_count on its /api/chat response (captured
    // directly here via serde). Either way, `tokens()` gives the count.
    #[serde(default)]
    pub total_tokens: Option<u64>,
    /// Input/context tokens (OpenAI `prompt_tokens`). Ollama reports the same via
    /// `prompt_eval_count` below.
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_eval_count: Option<i64>,
    #[serde(default)]
    pub eval_count: Option<i64>,
}

impl ChatResponse {
    /// Total tokens this call used, if the provider reported it.
    pub fn tokens(&self) -> Option<u64> {
        if let Some(t) = self.total_tokens {
            return Some(t);
        }
        if self.prompt_eval_count.is_some() || self.eval_count.is_some() {
            let p = self.prompt_eval_count.unwrap_or(0).max(0);
            let e = self.eval_count.unwrap_or(0).max(0);
            return Some((p + e) as u64);
        }
        None
    }

    /// Input/context tokens for this call (how full the context got), if reported.
    pub fn context_tokens(&self) -> Option<u64> {
        if let Some(p) = self.prompt_tokens {
            return Some(p);
        }
        self.prompt_eval_count.map(|c| c.max(0) as u64)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub modified_at: String,
    pub size: i64,
    pub digest: String,
    pub details: Option<ModelDetails>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelDetails {
    pub parent_model: Option<String>,
    pub format: Option<String>,
    pub family: Option<String>,
    pub families: Option<Vec<String>>,
    pub parameter_size: Option<String>,
    pub quantization_level: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ListModelsResponse {
    pub models: Vec<ModelInfo>,
}

#[derive(Clone)]
pub struct OllamaClient {
    client: Client,
    base_url: String,
}

impl OllamaClient {
    pub fn new(base_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self { client, base_url }
    }

    pub fn default_local() -> Self {
        Self::new("http://localhost:11434".to_string())
    }

    pub async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse> {
        let url = format!("{}/api/generate", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AgentaError::Ollama(format!("HTTP {}: {}", status, text)));
        }

        let result: GenerateResponse = response.json().await.map_err(|e| {
            AgentaError::Ollama(format!("Failed to parse response: {}", e))
        })?;

        Ok(result)
    }

    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/api/chat", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AgentaError::Ollama(format!("HTTP {}: {}", status, text)));
        }

        let result: ChatResponse = response.json().await.map_err(|e| {
            AgentaError::Ollama(format!("Failed to parse response: {}", e))
        })?;

        Ok(result)
    }

    /// Embed one or more texts. Returns one vector per input, in order.
    pub async fn embed(&self, model: &str, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/api/embed", self.base_url);
        let body = serde_json::json!({ "model": model, "input": texts });
        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Embed request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AgentaError::Ollama(format!("Embed HTTP {}: {}", status, text)));
        }

        #[derive(serde::Deserialize)]
        struct EmbedResponse {
            #[serde(default)]
            embeddings: Vec<Vec<f32>>,
        }
        let result: EmbedResponse = response.json().await.map_err(|e| {
            AgentaError::Ollama(format!("Failed to parse embed response: {}", e))
        })?;
        Ok(result.embeddings)
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let url = format!("{}/api/tags", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AgentaError::Ollama(format!("HTTP {}: {}", status, text)));
        }

        let result: ListModelsResponse = response.json().await.map_err(|e| {
            AgentaError::Ollama(format!("Failed to parse response: {}", e))
        })?;

        Ok(result.models)
    }

    pub async fn pull_model(&self, name: &str) -> Result<()> {
        let url = format!("{}/api/pull", self.base_url);
        let body = serde_json::json!({
            "name": name,
            "stream": false
        });

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(AgentaError::Ollama(format!("HTTP {}: {}", status, text)));
        }

        Ok(())
    }

    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/api/tags", self.base_url);
        match self.client.get(&url).send().await {
            Ok(response) => Ok(response.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}
