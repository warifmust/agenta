//! Knowledge bases + vector retrieval (RAG).
//!
//! v1 is pgvector-backed (Postgres). Vectors are encoded as pgvector's text form
//! (`[a,b,c]`) and cast with `::vector` in SQL, so no extra crate is needed.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgPoolOptions, Pool, Postgres, Row};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::core::{AgentaError, Result};

pub mod ingest;

/// v1 pins one embedding dimension (bge-m3 = 1024). The `knowledge_chunks.embedding`
/// column is `vector(1024)`; KBs of other dimensions are rejected for now.
pub const V1_DIMENSION: i32 = 1024;

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KnowledgeBase {
    pub id: String,
    pub name: String,
    pub embedder: String, // e.g. "ollama:bge-m3"
    pub dimension: i32,
    pub created_at: String,
}

/// A text chunk ready to store. `id` is a content hash so re-ingest is idempotent.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub id: String,
    pub source: String,
    pub chunk_index: i32,
    pub text: String,
    pub embedding: Vec<f32>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct RetrievedChunk {
    pub text: String,
    pub source: String,
    pub metadata: serde_json::Value,
    pub score: f32, // cosine similarity, higher = closer
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn create_kb(&self, name: &str, embedder: &str, dimension: i32) -> Result<KnowledgeBase>;
    async fn get_kb(&self, name: &str) -> Result<Option<KnowledgeBase>>;
    async fn list_kbs(&self) -> Result<Vec<KnowledgeBase>>;
    async fn delete_kb(&self, name: &str) -> Result<bool>;
    /// Insert chunks; existing ids (by content hash) are skipped. Returns rows inserted.
    async fn upsert_chunks(&self, kb_id: &str, chunks: &[Chunk]) -> Result<usize>;
    /// Of the given chunk ids, which already exist in this KB (so ingest can skip
    /// re-embedding them on resume).
    async fn existing_ids(&self, kb_id: &str, ids: &[String]) -> Result<HashSet<String>>;
    /// Top-k nearest chunks by cosine similarity.
    async fn search(&self, kb_id: &str, query: &[f32], k: usize) -> Result<Vec<RetrievedChunk>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::AppConfig;
    use crate::providers::build_embedder;

    // Needs Postgres (pgvector) + Ollama (bge-m3). Run explicitly:
    //   TEST_DATABASE_URL=postgres://… cargo test --lib pgvector_roundtrip -- --ignored
    #[tokio::test]
    #[ignore]
    async fn pgvector_roundtrip() {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("set TEST_DATABASE_URL to run this ignored integration test");
        let cfg = AppConfig::default();
        let emb = build_embedder(&cfg, "ollama:bge-m3").await.unwrap();
        let store = PgVectorStore::new(&url).await.unwrap();

        let _ = store.delete_kb("test-rag-kb").await; // clean slate
        let kb = store
            .create_kb("test-rag-kb", &emb.id(), emb.dimension() as i32)
            .await
            .unwrap();

        let docs = [
            "All praise is for Allah who gave us life after taking it from us — said when waking up.",
            "The reward of deeds depends upon the intentions — hadith of Umar.",
            "To make espresso, grind the beans finely and extract for about 25 seconds.",
        ];
        let vecs = emb
            .embed(&docs.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .await
            .unwrap();
        let chunks: Vec<Chunk> = docs
            .iter()
            .zip(vecs)
            .enumerate()
            .map(|(i, (t, v))| Chunk {
                id: format!("test-chunk-{}", i),
                source: "test.md".into(),
                chunk_index: i as i32,
                text: t.to_string(),
                embedding: v,
                metadata: serde_json::json!({ "page": i + 1 }),
            })
            .collect();

        assert_eq!(store.upsert_chunks(&kb.id, &chunks).await.unwrap(), 3);
        // idempotent: same content-hash ids are skipped
        assert_eq!(store.upsert_chunks(&kb.id, &chunks).await.unwrap(), 0);

        let q = emb
            .embed(&["what do I say when I wake up?".to_string()])
            .await
            .unwrap();
        let hits = store.search(&kb.id, &q[0], 3).await.unwrap();
        assert!(!hits.is_empty());
        assert!(
            hits[0].text.contains("waking up"),
            "top hit should be the waking-up dua, got: {}",
            hits[0].text
        );
        assert_eq!(hits[0].metadata["page"], 1);

        assert!(store.delete_kb("test-rag-kb").await.unwrap());
    }
}

/// Retrieve top-k passages across the given knowledge bases for a query, formatted
/// as a context block to inject into an agent's system prompt. Returns None if there
/// are no KBs, no Postgres, or no hits — retrieval failures are non-fatal to the run.
pub async fn retrieve_context(
    config: &crate::core::AppConfig,
    kb_names: &[String],
    query: &str,
    k: usize,
) -> Result<Option<String>> {
    let url = match &config.database_url {
        Some(u) if u.starts_with("postgres") => u.clone(),
        _ => return Ok(None),
    };
    let store = PgVectorStore::new(&url).await?;

    let mut hits: Vec<RetrievedChunk> = Vec::new();
    for name in kb_names {
        if let Some(kb) = store.get_kb(name).await? {
            let emb = crate::providers::build_embedder(config, &kb.embedder).await?;
            let qv = emb.embed(&[query.to_string()]).await?;
            if let Some(q) = qv.first() {
                hits.extend(store.search(&kb.id, q, k).await?);
            }
        }
    }
    if hits.is_empty() {
        return Ok(None);
    }
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(k);

    let mut block = String::from(
        "## Retrieved knowledge (answer ONLY from this)\n\
         Answer the question using ONLY the passages below. Cite the source and page for every \
         fact you state. If the passages do not contain the answer, say you don't have that in \
         the knowledge base — do NOT answer from your own knowledge, memory, or training, and do \
         NOT guess or fabricate (including any Arabic text).\n\n",
    );
    for h in &hits {
        let cite = match h.metadata.get("page").and_then(|p| p.as_i64()) {
            Some(p) => format!("({}, p.{})", h.source, p),
            None => format!("({})", h.source),
        };
        block.push_str(&format!("- {} {}\n", h.text.trim(), cite));
    }
    Ok(Some(block))
}

/// Encode a vector as pgvector's text form: `[0.1,0.2,...]`.
fn vec_to_pg(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

pub struct PgVectorStore {
    pool: Pool<Postgres>,
}

impl PgVectorStore {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        let store = Self { pool };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> Result<()> {
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS knowledge_bases (
                id         TEXT PRIMARY KEY,
                name       TEXT UNIQUE NOT NULL,
                embedder   TEXT NOT NULL,
                dimension  INTEGER NOT NULL,
                created_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(&format!(
            r#"
            CREATE TABLE IF NOT EXISTS knowledge_chunks (
                id          TEXT PRIMARY KEY,
                kb_id       TEXT NOT NULL REFERENCES knowledge_bases(id) ON DELETE CASCADE,
                source      TEXT NOT NULL,
                chunk_index INTEGER NOT NULL,
                chunk_text  TEXT NOT NULL,
                embedding   vector({dim}) NOT NULL,
                metadata    JSONB NOT NULL DEFAULT '{{}}',
                created_at  TEXT NOT NULL
            )
            "#,
            dim = V1_DIMENSION
        ))
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_embedding \
             ON knowledge_chunks USING hnsw (embedding vector_cosine_ops)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_knowledge_chunks_kb ON knowledge_chunks(kb_id)")
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}

#[async_trait]
impl VectorStore for PgVectorStore {
    async fn create_kb(&self, name: &str, embedder: &str, dimension: i32) -> Result<KnowledgeBase> {
        if dimension != V1_DIMENSION {
            return Err(AgentaError::Config(format!(
                "v1 supports {}-dim embedders only (got {})",
                V1_DIMENSION, dimension
            )));
        }
        let kb = KnowledgeBase {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            embedder: embedder.to_string(),
            dimension,
            created_at: Utc::now().to_rfc3339(),
        };
        sqlx::query(
            "INSERT INTO knowledge_bases (id, name, embedder, dimension, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&kb.id)
        .bind(&kb.name)
        .bind(&kb.embedder)
        .bind(kb.dimension)
        .bind(&kb.created_at)
        .execute(&self.pool)
        .await?;
        Ok(kb)
    }

    async fn get_kb(&self, name: &str) -> Result<Option<KnowledgeBase>> {
        let row = sqlx::query(
            "SELECT id, name, embedder, dimension, created_at FROM knowledge_bases WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| KnowledgeBase {
            id: r.get("id"),
            name: r.get("name"),
            embedder: r.get("embedder"),
            dimension: r.get("dimension"),
            created_at: r.get("created_at"),
        }))
    }

    async fn list_kbs(&self) -> Result<Vec<KnowledgeBase>> {
        let rows = sqlx::query(
            "SELECT id, name, embedder, dimension, created_at FROM knowledge_bases ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| KnowledgeBase {
                id: r.get("id"),
                name: r.get("name"),
                embedder: r.get("embedder"),
                dimension: r.get("dimension"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    async fn delete_kb(&self, name: &str) -> Result<bool> {
        let res = sqlx::query("DELETE FROM knowledge_bases WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn upsert_chunks(&self, kb_id: &str, chunks: &[Chunk]) -> Result<usize> {
        let mut inserted = 0usize;
        for c in chunks {
            let res = sqlx::query(
                "INSERT INTO knowledge_chunks \
                 (id, kb_id, source, chunk_index, chunk_text, embedding, metadata, created_at) \
                 VALUES ($1, $2, $3, $4, $5, $6::vector, $7, $8) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(&c.id)
            .bind(kb_id)
            .bind(&c.source)
            .bind(c.chunk_index)
            .bind(&c.text)
            .bind(vec_to_pg(&c.embedding))
            .bind(&c.metadata)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await?;
            inserted += res.rows_affected() as usize;
        }
        Ok(inserted)
    }

    async fn existing_ids(&self, kb_id: &str, ids: &[String]) -> Result<HashSet<String>> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let rows = sqlx::query(
            "SELECT id FROM knowledge_chunks WHERE kb_id = $1 AND id = ANY($2)",
        )
        .bind(kb_id)
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.get::<String, _>("id")).collect())
    }

    async fn search(&self, kb_id: &str, query: &[f32], k: usize) -> Result<Vec<RetrievedChunk>> {
        let rows = sqlx::query(
            "SELECT chunk_text, source, metadata, \
             1 - (embedding <=> $1::vector) AS score \
             FROM knowledge_chunks WHERE kb_id = $2 \
             ORDER BY embedding <=> $1::vector LIMIT $3",
        )
        .bind(vec_to_pg(query))
        .bind(kb_id)
        .bind(k as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| RetrievedChunk {
                text: r.get("chunk_text"),
                source: r.get("source"),
                metadata: r.get("metadata"),
                score: r.get::<f64, _>("score") as f32,
            })
            .collect())
    }
}
