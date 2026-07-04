use async_trait::async_trait;
use reqwest::Client;
use std::time::Duration;

use crate::core::{AgentaError, AppConfig, Result};

/// Default instruction for transcription. Emphasises exact reproduction (incl. Arabic
/// harakat) over translation/correction — critical for reference/religious text.
pub const DEFAULT_OCR_PROMPT: &str = "You are an OCR engine. Transcribe ALL text on this page \
    EXACTLY as it appears, including any Arabic script with its full harakat (tashkeel / \
    diacritics). Preserve the original text exactly — do NOT translate it, do NOT correct it, \
    and do NOT add or drop any harakat. Output only the raw transcription, preserving reading \
    order.";

/// Vision-model OCR: image bytes → text. Multi-provider (OpenRouter now; the same
/// OpenAI-compatible vision shape covers OpenAI/Gemini-via-OpenRouter).
#[async_trait]
pub trait Ocr: Send + Sync {
    fn id(&self) -> String;
    /// Transcribe a single page image (PNG). `prompt` overrides the default instruction.
    async fn ocr_image(&self, png: &[u8], prompt: Option<&str>) -> Result<String>;
}

/// OpenAI-compatible vision client (OpenRouter, OpenAI). Sends the image as a base64
/// data URL in a chat-completions request.
pub struct VisionOcr {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl VisionOcr {
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default();
        Self { client, base_url, api_key, model }
    }
}

#[async_trait]
impl Ocr for VisionOcr {
    fn id(&self) -> String {
        format!("{}", self.model)
    }

    async fn ocr_image(&self, png: &[u8], prompt: Option<&str>) -> Result<String> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let data_url = format!("data:image/png;base64,{}", STANDARD.encode(png));

        let body = serde_json::json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt.unwrap_or(DEFAULT_OCR_PROMPT) },
                    { "type": "image_url", "image_url": { "url": data_url } }
                ]
            }]
        });

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentaError::Ollama(format!("OCR request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentaError::Ollama(format!("OCR HTTP {}: {}", status, text)));
        }

        let parsed: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Failed to parse OCR response: {}", e)))?;

        parsed["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.trim().to_string())
            .ok_or_else(|| AgentaError::Ollama("OCR response missing content".to_string()))
    }
}

/// Build an OCR backend from a spec like "openrouter:qwen/qwen3-vl-32b-instruct".
pub fn build_ocr(config: &AppConfig, spec: &str) -> Result<Box<dyn Ocr>> {
    let (provider, model) = spec
        .split_once(':')
        .ok_or_else(|| AgentaError::Config(format!("OCR spec must be provider:model (got '{}')", spec)))?;

    match provider {
        "openrouter" => {
            let url = config
                .provider_url("openrouter")
                .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
            let key = config.provider_api_key("openrouter").unwrap_or_default();
            Ok(Box::new(VisionOcr::new(url, key, model.to_string())))
        }
        "openai" => {
            let url = config
                .provider_url("openai")
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let key = config.provider_api_key("openai").unwrap_or_default();
            Ok(Box::new(VisionOcr::new(url, key, model.to_string())))
        }
        other => Err(AgentaError::Config(format!(
            "Unsupported OCR provider '{}' (use openrouter or openai)",
            other
        ))),
    }
}
