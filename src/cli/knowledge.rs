use std::io::Write;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Result};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

use super::KnowledgeCommands;
use crate::core::AppConfig;
use crate::knowledge::{ingest, Chunk, PgVectorStore, VectorStore, V1_DIMENSION};
use crate::providers::build_embedder;

const EMBED_BATCH: usize = 64;

/// RAG requires Postgres/pgvector in v1.
fn pg_url(config: &AppConfig) -> Result<String> {
    match &config.database_url {
        Some(u) if u.starts_with("postgres") => Ok(u.clone()),
        _ => Err(anyhow!(
            "RAG requires Postgres. Set database_url = \"postgres://…\" in config.toml \
             (with the pgvector extension available)."
        )),
    }
}

pub async fn handle_knowledge_command(command: KnowledgeCommands, config: &AppConfig) -> Result<()> {
    let store = PgVectorStore::new(&pg_url(config)?).await?;

    match command {
        KnowledgeCommands::Create { name, embedder } => {
            let emb = build_embedder(config, &embedder).await?;
            let dim = emb.dimension() as i32;
            if dim != V1_DIMENSION {
                return Err(anyhow!(
                    "v1 supports {}-dim embedders only; '{}' is {}-dim",
                    V1_DIMENSION, embedder, dim
                ));
            }
            if store.get_kb(&name).await?.is_some() {
                return Err(anyhow!("Knowledge base '{}' already exists", name));
            }
            let kb = store.create_kb(&name, &emb.id(), dim).await?;
            println!(
                "{} Created knowledge base '{}' (embedder: {}, {}-dim)",
                "✓".green(), kb.name.bold(), kb.embedder, kb.dimension
            );
        }
        KnowledgeCommands::List => {
            let kbs = store.list_kbs().await?;
            if kbs.is_empty() {
                println!("{}", "No knowledge bases.".dimmed());
            }
            for kb in kbs {
                println!(
                    "  {}  ({}, {}-dim)  {}",
                    kb.name.bold().cyan(), kb.embedder, kb.dimension, kb.created_at.dimmed()
                );
            }
        }
        KnowledgeCommands::Remove { name } => {
            if store.delete_kb(&name).await? {
                println!("{} Removed knowledge base '{}'", "✓".green(), name);
            } else {
                println!("{}", format!("Knowledge base '{}' not found", name).yellow());
            }
        }
        KnowledgeCommands::Add { name, file, yes } => {
            add_file(config, &store, &name, &file, yes).await?;
        }
    }
    Ok(())
}

async fn add_file(
    config: &AppConfig,
    store: &PgVectorStore,
    kb_name: &str,
    file: &str,
    yes: bool,
) -> Result<()> {
    let kb = store.get_kb(kb_name).await?.ok_or_else(|| {
        anyhow!(
            "Knowledge base '{}' not found — create it first: agenta knowledge create {}",
            kb_name, kb_name
        )
    })?;

    let path = Path::new(file);
    if !path.exists() {
        return Err(anyhow!("File not found: {}", file));
    }
    let source = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file)
        .to_string();

    // 1. Extract (per page)
    println!("Extracting {}...", source.bold());
    let pages = ingest::extract_pages(path)?;
    let total_pages = pages.len();

    // 2. Preview → confirm (catches garbled extraction before embedding)
    let sample = ingest::preview_sample(&pages, 500);
    println!("\n{}", "── extracted text preview ───────────────".dimmed());
    println!("{}", sample);
    println!("{}\n", "─────────────────────────────────────────".dimmed());
    if !yes {
        print!("Looks right? Embed it? [y/N]: ");
        std::io::stdout().flush().ok();
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !ans.trim().eq_ignore_ascii_case("y") {
            println!("{}", "Aborted.".yellow());
            return Ok(());
        }
    }

    // 3. Chunk
    let raw = ingest::chunk_pages(&pages);
    if raw.is_empty() {
        return Err(anyhow!("No text extracted from {}", source));
    }

    // 4. Skip already-ingested chunks (resume)
    let ids: Vec<String> = raw
        .iter()
        .map(|c| ingest::chunk_id(&source, c.index, &c.text))
        .collect();
    let existing = store.existing_ids(&kb.id, &ids).await?;
    let todo: Vec<usize> = (0..raw.len())
        .filter(|i| !existing.contains(&ids[*i]))
        .collect();
    if todo.is_empty() {
        println!("{} Already ingested — {} chunks, nothing to do.", "✓".green(), raw.len());
        return Ok(());
    }
    if !existing.is_empty() {
        println!(
            "Resuming: {} of {} chunks already stored, embedding {} new.",
            existing.len(), raw.len(), todo.len()
        );
    }

    // 5. Embed in batches, with progress + resume-on-error
    let emb = build_embedder(config, &kb.embedder).await?;
    let pb = ProgressBar::new(todo.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  Embedding [{bar:32}] {pos}/{len} ({percent}%)  {per_sec}  ETA {eta}",
        )
        .unwrap()
        .progress_chars("=>-"),
    );
    let started = Instant::now();
    let mut added = 0usize;

    for batch in todo.chunks(EMBED_BATCH) {
        let texts: Vec<String> = batch.iter().map(|&i| raw[i].text.clone()).collect();
        let vecs = match emb.embed(&texts).await {
            Ok(v) => v,
            Err(e) => {
                pb.abandon();
                return Err(anyhow!(
                    "Embedding failed near chunk {} (≈ page {}): {}\n  Saved {} chunks so far — \
                     re-run `agenta knowledge add {} {}` to resume.",
                    batch[0], raw[batch[0]].page, e, added, kb_name, file
                ));
            }
        };
        let chunks: Vec<Chunk> = batch
            .iter()
            .zip(vecs)
            .map(|(&i, v)| Chunk {
                id: ids[i].clone(),
                source: source.clone(),
                chunk_index: raw[i].index,
                text: raw[i].text.clone(),
                embedding: v,
                metadata: serde_json::json!({ "page": raw[i].page }),
            })
            .collect();
        match store.upsert_chunks(&kb.id, &chunks).await {
            Ok(n) => added += n,
            Err(e) => {
                pb.abandon();
                return Err(anyhow!(
                    "Storing chunks failed near chunk {}: {}\n  Saved {} chunks — re-run to resume.",
                    batch[0], e, added
                ));
            }
        }
        pb.inc(batch.len() as u64);
    }
    pb.finish_and_clear();

    println!(
        "{} Added {} — {} pages, {} chunks embedded ({}) in {:.1}s",
        "✓".green(), source.bold(), total_pages, added, kb.embedder, started.elapsed().as_secs_f32()
    );
    Ok(())
}
