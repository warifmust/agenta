pub mod ollama;
pub mod openai_compat;

use async_trait::async_trait;

use crate::core::{AppConfig, Result};
use crate::ollama::client::{ChatRequest, ChatResponse, GenerateRequest, GenerateResponse};

/// Unified inference interface — every backend implements this.
#[async_trait]
pub trait ModelBackend: Send + Sync {
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse>;
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse>;
}

/// Build the right backend from global config + optional per-agent provider override.
/// Resolution order: agent provider > config default_provider > "ollama"
pub fn build_backend(
    config: &AppConfig,
    agent_provider: Option<&str>,
) -> std::sync::Arc<dyn ModelBackend> {
    let provider = agent_provider
        .unwrap_or(config.default_provider.as_deref().unwrap_or("ollama"));

    match provider {
        "openrouter" => {
            let url = config
                .provider_url("openrouter")
                .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
            let key = config.provider_api_key("openrouter").unwrap_or_default();
            std::sync::Arc::new(openai_compat::OpenAICompatClient::new(url, key))
        }
        "deepseek" => {
            let url = config
                .provider_url("deepseek")
                .unwrap_or_else(|| "https://api.deepseek.com/v1".to_string());
            let key = config.provider_api_key("deepseek").unwrap_or_default();
            std::sync::Arc::new(openai_compat::OpenAICompatClient::new(url, key))
        }
        "openai" => {
            let url = config
                .provider_url("openai")
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let key = config.provider_api_key("openai").unwrap_or_default();
            std::sync::Arc::new(openai_compat::OpenAICompatClient::new(url, key))
        }
        // "ollama" or anything unknown → fall back to Ollama
        _ => {
            let url = config
                .provider_url("ollama")
                .unwrap_or_else(|| config.ollama_url.clone());
            std::sync::Arc::new(ollama::OllamaBackend::new(url))
        }
    }
}
