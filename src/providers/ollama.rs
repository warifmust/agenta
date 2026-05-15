use async_trait::async_trait;

use crate::core::Result;
use crate::ollama::client::{ChatRequest, ChatResponse, GenerateRequest, GenerateResponse, OllamaClient};
use super::ModelBackend;

/// Ollama backend — wraps the existing OllamaClient unchanged.
pub struct OllamaBackend {
    client: OllamaClient,
}

impl OllamaBackend {
    pub fn new(url: String) -> Self {
        Self { client: OllamaClient::new(url) }
    }
}

#[async_trait]
impl ModelBackend for OllamaBackend {
    async fn generate(&self, request: GenerateRequest) -> Result<GenerateResponse> {
        self.client.generate(request).await
    }

    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse> {
        self.client.chat(request).await
    }
}
