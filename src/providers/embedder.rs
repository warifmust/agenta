use async_trait::async_trait;

use crate::core::{AgentaError, AppConfig, Result};
use crate::ollama::client::OllamaClient;

/// Text-embedding interface. Multi-provider (Ollama now; OpenAI/OpenRouter slot in
/// behind the same trait). The dimension is fixed by the model and pins the KB.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Stable identifier stored on the KB, e.g. "ollama:bge-m3".
    fn id(&self) -> String;
    /// Embedding dimension (e.g. 1024 for bge-m3).
    fn dimension(&self) -> usize;
    /// Embed a batch of texts; returns one vector per input, in order.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Ollama-backed embedder (e.g. bge-m3).
pub struct OllamaEmbedder {
    client: OllamaClient,
    model: String,
    dimension: usize,
}

impl OllamaEmbedder {
    pub fn new(base_url: String, model: String, dimension: usize) -> Self {
        Self { client: OllamaClient::new(base_url), model, dimension }
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    fn id(&self) -> String {
        format!("ollama:{}", self.model)
    }
    fn dimension(&self) -> usize {
        self.dimension
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        self.client.embed(&self.model, texts).await
    }
}

/// OpenAI-compatible embedder (OpenRouter, OpenAI). Requests `dimensions` = the
/// target vector size so the output fits agenta's fixed `vector(1024)` schema —
/// `text-embedding-3-*` support this natively.
pub struct OpenAIEmbedder {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    dimension: usize,
    id: String,
}

impl OpenAIEmbedder {
    pub fn new(base_url: String, api_key: String, model: String, dimension: usize, id: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self { client, base_url, api_key, model, dimension, id }
    }
}

#[async_trait]
impl Embedder for OpenAIEmbedder {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn dimension(&self) -> usize {
        self.dimension
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
            "dimensions": self.dimension,
        });
        let resp = self
            .client
            .post(format!("{}/embeddings", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Embed request failed: {}", e)))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AgentaError::Ollama(format!("Embed HTTP {}: {}", status, text)));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AgentaError::Ollama(format!("Failed to parse embed response: {}", e)))?;

        // { "data": [ { "embedding": [...], "index": 0 }, ... ] } — order by index.
        let mut items: Vec<(usize, Vec<f32>)> = v["data"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| {
                        let idx = d["index"].as_u64().unwrap_or(0) as usize;
                        let emb: Vec<f32> = d["embedding"]
                            .as_array()?
                            .iter()
                            .filter_map(|x| x.as_f64().map(|f| f as f32))
                            .collect();
                        Some((idx, emb))
                    })
                    .collect()
            })
            .unwrap_or_default();
        items.sort_by_key(|(i, _)| *i);
        Ok(items.into_iter().map(|(_, e)| e).collect())
    }
}

/// Known embedding models → dimension. Used to pin a KB's vector size without a
/// round-trip. Unknown models fall back to a probe (embed one token, measure).
fn known_dimension(model: &str) -> Option<usize> {
    match model {
        "bge-m3" => Some(1024),
        "nomic-embed-text" => Some(768),
        "mxbai-embed-large" => Some(1024),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hits a live Ollama with bge-m3 pulled — run explicitly:
    //   cargo test --lib ollama_bge_m3_embeds -- --ignored
    #[tokio::test]
    #[ignore]
    async fn ollama_bge_m3_embeds() {
        let cfg = AppConfig::default();
        let emb = build_embedder(&cfg, "ollama:bge-m3").await.unwrap();
        assert_eq!(emb.id(), "ollama:bge-m3");
        assert_eq!(emb.dimension(), 1024);

        let out = emb
            .embed(&[
                "when waking up".to_string(),
                "hadith about intentions".to_string(),
            ])
            .await
            .unwrap();
        assert_eq!(out.len(), 2, "one vector per input");
        assert_eq!(out[0].len(), 1024, "bge-m3 is 1024-dim");
        assert_eq!(out[1].len(), 1024);
    }
}

/// Verify the embedder is usable before doing expensive work (e.g. a 20-minute OCR
/// pass). Ollama: model must be installed (returns a clear "ollama pull" hint). Cloud:
/// the API key must be set and the model must return the required 1024-dim vectors.
pub async fn ensure_embedder_available(config: &AppConfig, spec: &str) -> Result<()> {
    let (provider, model) = spec.split_once(':').unwrap_or(("ollama", spec));

    if provider == "openai" || provider == "openrouter" {
        let key = config.provider_api_key(provider).unwrap_or_default();
        if key.is_empty() {
            return Err(AgentaError::Config(format!(
                "No API key configured for '{}'. Add it to ~/.agenta/.env and reference it in config.toml.",
                provider
            )));
        }
        // Probe once: validates key + endpoint + that the model produces 1024-dim vectors.
        let emb = build_embedder(config, spec).await?;
        let dim = emb
            .embed(&["dimension check".to_string()])
            .await?
            .first()
            .map(|e| e.len())
            .unwrap_or(0);
        if dim != TARGET_DIMENSION {
            return Err(AgentaError::Config(format!(
                "Embedder '{}' returned {}-dim vectors; agenta requires {} (use a model that supports \
                 the 'dimensions' parameter, e.g. openai/text-embedding-3-large).",
                spec, dim, TARGET_DIMENSION
            )));
        }
        return Ok(());
    }

    let url = config
        .provider_url("ollama")
        .unwrap_or_else(|| config.ollama_url.clone());
    let installed = OllamaClient::new(url).list_models().await.map_err(|e| {
        AgentaError::Ollama(format!(
            "Could not reach Ollama to check for model '{}': {}",
            model, e
        ))
    })?;

    let found = installed.iter().any(|m| {
        m.name == model
            || m.name == format!("{}:latest", model)
            || m.name.starts_with(&format!("{}:", model))
    });
    if found {
        Ok(())
    } else {
        Err(AgentaError::Config(format!(
            "Embedding model '{}' is not installed in Ollama.\n  Install it with:  ollama pull {}",
            model, model
        )))
    }
}

/// Build an embedder from a spec like "ollama:bge-m3". Provider defaults to Ollama.
/// v1 implements Ollama; OpenAI/OpenRouter are additive behind the same trait.
pub async fn build_embedder(
    config: &AppConfig,
    spec: &str,
) -> Result<Box<dyn Embedder>> {
    let (provider, model) = spec.split_once(':').unwrap_or(("ollama", spec));

    match provider {
        "openrouter" => {
            let url = config
                .provider_url("openrouter")
                .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
            let key = config.provider_api_key("openrouter").unwrap_or_default();
            Ok(Box::new(OpenAIEmbedder::new(
                url, key, model.to_string(), TARGET_DIMENSION, spec.to_string(),
            )))
        }
        "openai" => {
            let url = config
                .provider_url("openai")
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let key = config.provider_api_key("openai").unwrap_or_default();
            Ok(Box::new(OpenAIEmbedder::new(
                url, key, model.to_string(), TARGET_DIMENSION, spec.to_string(),
            )))
        }
        // "ollama" or anything unknown → Ollama
        _ => {
            let url = config
                .provider_url("ollama")
                .unwrap_or_else(|| config.ollama_url.clone());
            let dimension = match known_dimension(model) {
                Some(d) => d,
                None => {
                    // Probe: embed a single token and measure the returned vector.
                    let client = OllamaClient::new(url.clone());
                    let v = client.embed(model, &["dimension probe".to_string()]).await?;
                    v.first().map(|e| e.len()).unwrap_or(0)
                }
            };
            Ok(Box::new(OllamaEmbedder::new(url, model.to_string(), dimension)))
        }
    }
}

/// Vector dimension every KB must produce, matching the fixed `vector(1024)` schema.
/// Cloud embedders are requested at this size (via the `dimensions` parameter).
const TARGET_DIMENSION: usize = 1024;
