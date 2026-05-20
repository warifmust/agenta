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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{AppConfig, types::ProviderConfig};

    fn cfg_with_provider(name: &str, url: &str, key: &str) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.providers.insert(
            name.to_string(),
            ProviderConfig {
                url: Some(url.to_string()),
                api_key: Some(key.to_string()),
            },
        );
        cfg
    }

    /// build_backend must not panic for any supported provider string.
    #[test]
    fn build_backend_ollama_default() {
        let cfg = AppConfig::default();
        // Should build without panic; we can't call the backend without a server,
        // but we verify the Arc is created.
        let _backend = build_backend(&cfg, None);
    }

    #[test]
    fn build_backend_deepseek_provider() {
        let cfg = cfg_with_provider("deepseek", "https://api.deepseek.com/v1", "sk-test");
        let _backend = build_backend(&cfg, Some("deepseek"));
    }

    #[test]
    fn build_backend_openrouter_provider() {
        let cfg = cfg_with_provider("openrouter", "https://openrouter.ai/api/v1", "sk-or-test");
        let _backend = build_backend(&cfg, Some("openrouter"));
    }

    #[test]
    fn build_backend_openai_provider() {
        let cfg = cfg_with_provider("openai", "https://api.openai.com/v1", "sk-oai-test");
        let _backend = build_backend(&cfg, Some("openai"));
    }

    #[test]
    fn build_backend_agent_provider_overrides_default() {
        // Config default is "ollama", but agent specifies "deepseek" — must use deepseek
        let cfg = cfg_with_provider("deepseek", "https://api.deepseek.com/v1", "sk-test");
        // No panic and backend is created; URL is validated via provider_url
        let _backend = build_backend(&cfg, Some("deepseek"));
    }

    #[test]
    fn build_backend_unknown_provider_falls_back_to_ollama() {
        let cfg = AppConfig::default();
        // Completely unknown provider — should silently fall back to Ollama
        let _backend = build_backend(&cfg, Some("unknown-provider-xyz"));
    }
}
