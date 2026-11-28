//! Qdrant-backed vector store for RAG knowledge bases.
//!
//! Uses Qdrant's REST API over `reqwest` (no extra crate dependency).
//! - Each knowledge base maps to its own Qdrant collection (`<prefix><sanitized_name>`)
//!   with Cosine distance and the KB's native embedding dimension (not locked to 1024).
//! - Knowledge base metadata (`name`, `embedder`, `dimension`, `created_at`) is stored
//!   in a small registry collection (`<prefix>__registry`).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

use async_trait::async_trait;
use chrono::Utc;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{json, Value};

use crate::core::{AgentaError, Result};
use crate::knowledge::{Chunk, KnowledgeBase, RetrievedChunk, VectorStore};

const REGISTRY_SUFFIX: &str = "__registry";

pub struct QdrantVectorStore {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    collection_prefix: String,
}

impl QdrantVectorStore {
    pub async fn new(
        base_url: &str,
        api_key: Option<String>,
        collection_prefix: &str,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| AgentaError::Config(format!("Failed to build Qdrant HTTP client: {}", e)))?;

        let store = Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            collection_prefix: collection_prefix.to_string(),
        };
        store.ensure_registry().await?;
        Ok(store)
    }

    fn registry_collection(&self) -> String {
        format!("{}{}", self.collection_prefix, REGISTRY_SUFFIX)
    }

    fn collection_for_kb(&self, name: &str) -> String {
        let sanitized: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let trimmed = sanitized.trim_matches('_');
        let suffix = if trimmed.is_empty() {
            format!("{:016x}", hash_str(name))
        } else {
            trimmed.to_string()
        };
        format!("{}{}", self.collection_prefix, suffix)
    }

    fn auth(&self, req: RequestBuilder) -> RequestBuilder {
        match &self.api_key {
            Some(key) if !key.is_empty() => req.header("api-key", key),
            _ => req,
        }
    }

    async fn ensure_registry(&self) -> Result<()> {
        let reg = self.registry_collection();
        let url = format!("{}/collections/{}", self.base_url, reg);
        let resp = self
            .auth(self.client.get(&url))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Could not reach Qdrant at {}: {}", self.base_url, e)))?;

        if resp.status().is_success() {
            return Ok(());
        }
        if resp.status() != StatusCode::NOT_FOUND {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Qdrant registry check failed (HTTP {}): {}",
                status, body
            )));
        }

        let create_resp = self
            .auth(self.client.put(&url))
            .json(&json!({
                "vectors": {
                    "size": 1,
                    "distance": "Cosine"
                }
            }))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Failed to create Qdrant registry: {}", e)))?;

        if !create_resp.status().is_success() {
            let status = create_resp.status();
            let body = create_resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Failed to create Qdrant registry (HTTP {}): {}",
                status, body
            )));
        }
        Ok(())
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Convert a chunk string ID (e.g. 16-char hex from `ingest::chunk_id`, or arbitrary string)
/// into a Qdrant-compatible unsigned 64-bit integer point ID.
fn chunk_id_to_point_id(id: &str) -> u64 {
    if id.len() == 16 {
        if let Ok(v) = u64::from_str_radix(id, 16) {
            return v;
        }
    }
    hash_str(id)
}

fn parse_kb_from_payload(payload: &Value) -> Option<KnowledgeBase> {
    Some(KnowledgeBase {
        id: payload.get("id")?.as_str()?.to_string(),
        name: payload.get("name")?.as_str()?.to_string(),
        embedder: payload.get("embedder")?.as_str()?.to_string(),
        dimension: payload.get("dimension")?.as_i64()? as i32,
        created_at: payload.get("created_at")?.as_str()?.to_string(),
    })
}

#[async_trait]
impl VectorStore for QdrantVectorStore {
    async fn create_kb(&self, name: &str, embedder: &str, dimension: i32) -> Result<KnowledgeBase> {
        if dimension <= 0 {
            return Err(AgentaError::Config(format!(
                "Invalid embedding dimension: {}",
                dimension
            )));
        }
        if self.get_kb(name).await?.is_some() {
            return Err(AgentaError::Config(format!(
                "Knowledge base '{}' already exists",
                name
            )));
        }

        let collection = self.collection_for_kb(name);
        let col_url = format!("{}/collections/{}", self.base_url, collection);
        let resp = self
            .auth(self.client.put(&col_url))
            .json(&json!({
                "vectors": {
                    "size": dimension,
                    "distance": "Cosine"
                }
            }))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Qdrant create collection failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Qdrant create collection '{}' failed (HTTP {}): {}",
                collection, status, body
            )));
        }

        let kb = KnowledgeBase {
            id: collection,
            name: name.to_string(),
            embedder: embedder.to_string(),
            dimension,
            created_at: Utc::now().to_rfc3339(),
        };

        let reg_url = format!(
            "{}/collections/{}/points?wait=true",
            self.base_url,
            self.registry_collection()
        );
        let reg_resp = self
            .auth(self.client.put(&reg_url))
            .json(&json!({
                "points": [{
                    "id": hash_str(name),
                    "vector": [0.0],
                    "payload": {
                        "id": kb.id,
                        "name": kb.name,
                        "embedder": kb.embedder,
                        "dimension": kb.dimension,
                        "created_at": kb.created_at,
                    }
                }]
            }))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Qdrant registry upsert failed: {}", e)))?;

        if !reg_resp.status().is_success() {
            let status = reg_resp.status();
            let body = reg_resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Qdrant registry upsert failed (HTTP {}): {}",
                status, body
            )));
        }

        Ok(kb)
    }

    async fn get_kb(&self, name: &str) -> Result<Option<KnowledgeBase>> {
        let url = format!(
            "{}/collections/{}/points",
            self.base_url,
            self.registry_collection()
        );
        let resp = self
            .auth(self.client.post(&url))
            .json(&json!({
                "ids": [hash_str(name)],
                "with_payload": true,
                "with_vector": false
            }))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Qdrant get_kb failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Qdrant get_kb failed (HTTP {}): {}",
                status, body
            )));
        }

        let v: Value = resp
            .json()
            .await
            .map_err(|e| AgentaError::Config(format!("Invalid Qdrant response: {}", e)))?;

        let kb = v["result"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|pt| pt.get("payload"))
            .and_then(parse_kb_from_payload)
            .filter(|kb| kb.name == name);

        Ok(kb)
    }

    async fn list_kbs(&self) -> Result<Vec<KnowledgeBase>> {
        let url = format!(
            "{}/collections/{}/points/scroll",
            self.base_url,
            self.registry_collection()
        );
        let mut kbs = Vec::new();
        let mut next_offset: Option<Value> = None;

        loop {
            let mut body = json!({
                "limit": 256,
                "with_payload": true,
                "with_vector": false
            });
            if let Some(offset) = &next_offset {
                body["offset"] = offset.clone();
            }

            let resp = self
                .auth(self.client.post(&url))
                .json(&body)
                .send()
                .await
                .map_err(|e| AgentaError::Config(format!("Qdrant list_kbs failed: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(AgentaError::Config(format!(
                    "Qdrant list_kbs failed (HTTP {}): {}",
                    status, text
                )));
            }

            let v: Value = resp
                .json()
                .await
                .map_err(|e| AgentaError::Config(format!("Invalid Qdrant scroll response: {}", e)))?;

            if let Some(points) = v["result"]["points"].as_array() {
                for pt in points {
                    if let Some(kb) = pt.get("payload").and_then(parse_kb_from_payload) {
                        kbs.push(kb);
                    }
                }
            }

            match v["result"].get("next_page_offset") {
                Some(off) if !off.is_null() => next_offset = Some(off.clone()),
                _ => break,
            }
        }

        kbs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(kbs)
    }

    async fn delete_kb(&self, name: &str) -> Result<bool> {
        let Some(kb) = self.get_kb(name).await? else {
            return Ok(false);
        };

        // Delete the KB's collection (ignore 404 if already removed externally).
        let col_url = format!("{}/collections/{}", self.base_url, kb.id);
        let resp = self
            .auth(self.client.delete(&col_url))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Qdrant delete collection failed: {}", e)))?;
        if !resp.status().is_success() && resp.status() != StatusCode::NOT_FOUND {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Qdrant delete collection '{}' failed (HTTP {}): {}",
                kb.id, status, body
            )));
        }

        // Remove from registry.
        let reg_url = format!(
            "{}/collections/{}/points/delete?wait=true",
            self.base_url,
            self.registry_collection()
        );
        let del_resp = self
            .auth(self.client.post(&reg_url))
            .json(&json!({
                "points": [hash_str(name)]
            }))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Qdrant registry delete failed: {}", e)))?;
        if !del_resp.status().is_success() {
            let status = del_resp.status();
            let body = del_resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Qdrant registry delete failed (HTTP {}): {}",
                status, body
            )));
        }

        Ok(true)
    }

    async fn upsert_chunks(&self, kb_id: &str, chunks: &[Chunk]) -> Result<usize> {
        if chunks.is_empty() {
            return Ok(0);
        }

        // Skip existing IDs to match PgVectorStore's idempotent ON CONFLICT DO NOTHING semantics.
        let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
        let existing = self.existing_ids(kb_id, &ids).await?;

        let now = Utc::now().to_rfc3339();
        let points: Vec<Value> = chunks
            .iter()
            .filter(|c| !existing.contains(&c.id))
            .map(|c| {
                json!({
                    "id": chunk_id_to_point_id(&c.id),
                    "vector": c.embedding,
                    "payload": {
                        "chunk_id": c.id,
                        "source": c.source,
                        "chunk_index": c.chunk_index,
                        "chunk_text": c.text,
                        "metadata": c.metadata,
                        "created_at": now,
                    }
                })
            })
            .collect();

        if points.is_empty() {
            return Ok(0);
        }

        let count = points.len();
        let url = format!("{}/collections/{}/points?wait=true", self.base_url, kb_id);
        let resp = self
            .auth(self.client.put(&url))
            .json(&json!({ "points": points }))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Qdrant upsert_chunks failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Qdrant upsert_chunks failed (HTTP {}): {}",
                status, body
            )));
        }

        Ok(count)
    }

    async fn existing_ids(&self, kb_id: &str, ids: &[String]) -> Result<HashSet<String>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }

        let url = format!("{}/collections/{}/points", self.base_url, kb_id);
        let mut found = HashSet::new();

        // Batch point lookups to keep payloads reasonable for large documents.
        for batch in ids.chunks(500) {
            let point_ids: Vec<u64> = batch.iter().map(|id| chunk_id_to_point_id(id)).collect();
            let resp = self
                .auth(self.client.post(&url))
                .json(&json!({
                    "ids": point_ids,
                    "with_payload": ["chunk_id"],
                    "with_vector": false
                }))
                .send()
                .await
                .map_err(|e| AgentaError::Config(format!("Qdrant existing_ids failed: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(AgentaError::Config(format!(
                    "Qdrant existing_ids failed (HTTP {}): {}",
                    status, body
                )));
            }

            let v: Value = resp
                .json()
                .await
                .map_err(|e| AgentaError::Config(format!("Invalid Qdrant points response: {}", e)))?;

            if let Some(arr) = v["result"].as_array() {
                for pt in arr {
                    if let Some(cid) = pt["payload"]["chunk_id"].as_str() {
                        found.insert(cid.to_string());
                    }
                }
            }
        }

        Ok(found)
    }

    async fn search(&self, kb_id: &str, query: &[f32], k: usize) -> Result<Vec<RetrievedChunk>> {
        let url = format!("{}/collections/{}/points/search", self.base_url, kb_id);
        let resp = self
            .auth(self.client.post(&url))
            .json(&json!({
                "vector": query,
                "limit": k,
                "with_payload": true
            }))
            .send()
            .await
            .map_err(|e| AgentaError::Config(format!("Qdrant search failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentaError::Config(format!(
                "Qdrant search failed (HTTP {}): {}",
                status, body
            )));
        }

        let v: Value = resp
            .json()
            .await
            .map_err(|e| AgentaError::Config(format!("Invalid Qdrant search response: {}", e)))?;

        let hits = v["result"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|pt| {
                        let payload = pt.get("payload")?;
                        Some(RetrievedChunk {
                            text: payload.get("chunk_text")?.as_str()?.to_string(),
                            source: payload
                                .get("source")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string(),
                            metadata: payload
                                .get("metadata")
                                .cloned()
                                .unwrap_or_else(|| json!({})),
                            score: pt.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0) as f32,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_chunk_id_maps_deterministically_to_u64() {
        let id = "0123456789abcdef";
        assert_eq!(chunk_id_to_point_id(id), 0x0123456789abcdef);
        assert_eq!(chunk_id_to_point_id("custom-id"), chunk_id_to_point_id("custom-id"));
    }

    #[test]
    fn sanitizes_kb_names_into_valid_qdrant_collections() {
        let store = QdrantVectorStore {
            client: Client::new(),
            base_url: "http://127.0.0.1:6333".into(),
            api_key: None,
            collection_prefix: "agenta_kb_".into(),
        };
        assert_eq!(store.collection_for_kb("Sahih Bukhari"), "agenta_kb_sahih_bukhari");
        assert_eq!(store.collection_for_kb("hisnul-muslim"), "agenta_kb_hisnul-muslim");
    }

    // Hits a live Qdrant instance — run explicitly:
    //   TEST_QDRANT_URL=http://192.168.100.217:6333 cargo test --lib qdrant_roundtrip -- --ignored
    #[tokio::test]
    #[ignore]
    async fn qdrant_roundtrip() {
        let url = std::env::var("TEST_QDRANT_URL")
            .expect("set TEST_QDRANT_URL to run this ignored integration test");
        let prefix = format!("agenta_test_{}_", &uuid::Uuid::new_v4().to_string()[..8]);
        let store = QdrantVectorStore::new(&url, None, &prefix).await.unwrap();

        // 1. Create KB with non-1024 dimension (proves variable-dim support)
        let kb = store.create_kb("test-hadith-kb", "ollama:test-embed", 4).await.unwrap();
        assert_eq!(kb.name, "test-hadith-kb");
        assert_eq!(kb.dimension, 4);

        // 2. get_kb & list_kbs
        let fetched = store.get_kb("test-hadith-kb").await.unwrap().expect("kb exists");
        assert_eq!(fetched.id, kb.id);
        let listed = store.list_kbs().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "test-hadith-kb");

        // 3. upsert_chunks + idempotency
        let chunks = vec![
            Chunk {
                id: "0000000000000001".into(),
                source: "bukhari.md".into(),
                chunk_index: 0,
                text: "Actions are judged by intentions.".into(),
                embedding: vec![1.0, 0.0, 0.0, 0.0],
                metadata: json!({ "book": "Sahih al-Bukhari", "hadith": 1 }),
            },
            Chunk {
                id: "0000000000000002".into(),
                source: "coffee.md".into(),
                chunk_index: 1,
                text: "Grind espresso beans finely.".into(),
                embedding: vec![0.0, 1.0, 0.0, 0.0],
                metadata: json!({ "page": 2 }),
            },
        ];
        assert_eq!(store.upsert_chunks(&kb.id, &chunks).await.unwrap(), 2);
        // Re-inserting same IDs must be a no-op
        assert_eq!(store.upsert_chunks(&kb.id, &chunks).await.unwrap(), 0);

        // 4. existing_ids
        let existing = store
            .existing_ids(&kb.id, &["0000000000000001".into(), "0000000000000099".into()])
            .await
            .unwrap();
        assert!(existing.contains("0000000000000001"));
        assert!(!existing.contains("0000000000000099"));

        // 5. search
        let hits = store.search(&kb.id, &[0.99, 0.01, 0.0, 0.0], 2).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits[0].text.contains("intentions"));
        assert_eq!(hits[0].source, "bukhari.md");
        assert_eq!(hits[0].metadata["hadith"], 1);
        assert!(hits[0].score > hits[1].score);

        // 6. delete_kb + cleanup temporary test registry collection
        assert!(store.delete_kb("test-hadith-kb").await.unwrap());
        assert!(!store.delete_kb("test-hadith-kb").await.unwrap());
        assert!(store.get_kb("test-hadith-kb").await.unwrap().is_none());

        let reg_url = format!("{}/collections/{}", store.base_url, store.registry_collection());
        let _ = store.client.delete(&reg_url).send().await;
    }
}
