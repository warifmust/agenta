use std::io::Write;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Result};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

use super::KnowledgeCommands;
use crate::core::AppConfig;
use crate::knowledge::{ingest, Chunk, PgVectorStore, VectorStore, V1_DIMENSION};
use crate::providers::{build_embedder, build_ocr, ensure_embedder_available};

const EMBED_BATCH: usize = 64;

/// Render a PDF's pages to images (pdftoppm), OCR each with a vision model, and return
/// them as pages. The first page is OCR'd for a preview/confirm before the rest, so we
/// don't spend a vision call per page before you can abort. `None` = user aborted.
async fn ocr_pages(
    config: &AppConfig,
    path: &Path,
    spec: &str,
    yes: bool,
) -> Result<Option<Vec<ingest::Page>>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext != "pdf" {
        return Err(anyhow!("--ocr only supports PDF files (got .{})", ext));
    }
    if std::process::Command::new("pdftoppm").arg("-h").output().is_err() {
        return Err(anyhow!(
            "pdftoppm not found — install poppler to render PDF pages for OCR \
             (macOS: brew install poppler)."
        ));
    }

    // Total page count up front (pdfinfo ships with poppler alongside pdftoppm).
    let total = pdf_page_count(path).ok_or_else(|| {
        anyhow!("Could not read page count — ensure poppler's `pdfinfo` is installed (macOS: brew install poppler).")
    })?;
    if total == 0 {
        return Err(anyhow!("{} has no pages", path.display()));
    }

    // Work in a temp dir. We render and OCR ONE page at a time, which (a) shows real
    // progress instead of a silent multi-minute upfront render, (b) doesn't render
    // 800+ pages before you've even seen the preview, and (c) lets us SKIP a page
    // that fails OCR instead of throwing away the whole (already paid-for) run.
    let tmp = std::env::temp_dir().join(format!("agenta-ocr-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp)?;

    // Render a single 1-based page to PNG bytes (`-singlefile` gives a predictable name).
    let render_page = |n: usize| -> Result<Vec<u8>> {
        let prefix = tmp.join("current");
        let status = std::process::Command::new("pdftoppm")
            .args(["-f", &n.to_string(), "-l", &n.to_string(), "-singlefile", "-png", "-r", "200"])
            .arg(path)
            .arg(&prefix)
            .status()?;
        if !status.success() {
            return Err(anyhow!("pdftoppm failed to render page {}", n));
        }
        let png = prefix.with_extension("png");
        let bytes = std::fs::read(&png)?;
        let _ = std::fs::remove_file(&png);
        Ok(bytes)
    };

    let ocr = build_ocr(config, spec)?;

    // ── Page 1 → preview → confirm (only one page rendered so far — fast) ──
    println!("Rendering page 1 for a preview...");
    let first_png = render_page(1).inspect_err(|_| { let _ = std::fs::remove_dir_all(&tmp); })?;
    let first = match ocr.ocr_image(&first_png, None).await {
        Ok(t) => t,
        Err(e) => { let _ = std::fs::remove_dir_all(&tmp); return Err(anyhow!("OCR failed on page 1: {}", e)); }
    };
    println!("\n{}", "── OCR text preview (page 1) ────────────".dimmed());
    println!("{}", first.chars().take(600).collect::<String>());
    println!("{}\n", "─────────────────────────────────────────".dimmed());
    if !yes {
        print!("OCR looks right? Embed all {} pages? [y/N]: ", total);
        std::io::stdout().flush().ok();
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !ans.trim().eq_ignore_ascii_case("y") {
            let _ = std::fs::remove_dir_all(&tmp);
            return Ok(None);
        }
    }

    // ── Render + OCR the rest, one page at a time, with a live progress bar ──
    println!("OCR via {} ({} pages)...", spec.bold(), total);
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "  [{bar:32}] {pos}/{len} pages ({percent}%)  {per_sec}  ETA {eta}",
        )
        .unwrap()
        .progress_chars("=>-"),
    );
    let mut pages = vec![ingest::Page { number: 1, text: first }];
    pb.inc(1);
    let mut failed: Vec<usize> = Vec::new();

    for n in 2..=total {
        let png = match render_page(n) {
            Ok(b) => b,
            Err(e) => { pb.println(format!("  ! page {} render failed: {} — skipped", n, e)); failed.push(n); pb.inc(1); continue; }
        };
        // One retry — "OCR response missing content" and rate blips are often transient.
        let mut text: Option<String> = None;
        for attempt in 0..2 {
            match ocr.ocr_image(&png, None).await {
                Ok(t) => { text = Some(t); break; }
                Err(_) if attempt == 0 => tokio::time::sleep(std::time::Duration::from_millis(800)).await,
                Err(e) => pb.println(format!("  ! page {} OCR failed after retry: {} — skipped", n, e)),
            }
        }
        match text {
            Some(t) => pages.push(ingest::Page { number: n as i32, text: t }),
            None => failed.push(n),
        }
        pb.inc(1);
    }
    pb.finish_and_clear();
    let _ = std::fs::remove_dir_all(&tmp);

    if !failed.is_empty() {
        println!(
            "{} {} of {} pages failed OCR and were skipped: {}",
            "!".yellow(),
            failed.len(),
            total,
            failed.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ")
        );
    }
    println!("{} OCR done — captured {}/{} pages.", "✓".green(), pages.len(), total);
    Ok(Some(pages))
}

/// Read a PDF's page count via poppler's `pdfinfo`. None if it's unavailable or
/// the output can't be parsed.
fn pdf_page_count(path: &Path) -> Option<usize> {
    let out = std::process::Command::new("pdfinfo").arg(path).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("Pages:").and_then(|r| r.trim().parse().ok()))
}

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
            // Fail fast if the embedder model isn't available (don't create a KB
            // bound to a model that isn't installed).
            ensure_embedder_available(config, &embedder).await?;
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
            } else {
                let mut table = crate::cli::commands::styled_table();
                table.set_header(vec!["Name", "Embedder", "Dim", "Created"]);
                for kb in kbs {
                    let created = kb
                        .created_at
                        .get(0..16)
                        .unwrap_or(&kb.created_at)
                        .replace('T', " ");
                    table.add_row(vec![
                        kb.name,
                        kb.embedder,
                        kb.dimension.to_string(),
                        created,
                    ]);
                }
                println!("{table}");
            }
        }
        KnowledgeCommands::Remove { name } => {
            if store.delete_kb(&name).await? {
                println!("{} Removed knowledge base '{}'", "✓".green(), name);
            } else {
                println!("{}", format!("Knowledge base '{}' not found", name).yellow());
            }
        }
        KnowledgeCommands::Add { name, file, yes, ocr, chunk_strategy } => {
            let strategy = ingest::ChunkStrategy::parse(&chunk_strategy).ok_or_else(|| {
                anyhow!("Invalid --chunk-strategy '{}' (use 'words' or 'entries')", chunk_strategy)
            })?;
            add_file(config, &store, &name, &file, yes, ocr, strategy).await?;
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
    ocr: Option<String>,
    strategy: ingest::ChunkStrategy,
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

    // Verify the KB's embedder is available BEFORE any expensive work (e.g. OCR):
    // embedding runs after extraction, so a missing model would otherwise fail only
    // after minutes of OCR.
    ensure_embedder_available(config, &kb.embedder).await?;
    let source = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file)
        .to_string();

    // 1. Get pages — either text extraction or vision-model OCR. OCR does its own
    // first-page preview/confirm (so we don't burn a vision call per page before you
    // can abort); the text path previews below.
    let pages = match &ocr {
        Some(spec) => match ocr_pages(config, path, spec, yes).await? {
            Some(p) => p,
            None => {
                println!("{}", "Aborted.".yellow());
                return Ok(());
            }
        },
        None => {
            println!("Extracting {}...", source.bold());
            let pages = ingest::extract_pages(path)?;
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
            pages
        }
    };
    let total_pages = pages.len();

    // 3. Chunk
    let raw = ingest::chunk(&pages, strategy);
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
