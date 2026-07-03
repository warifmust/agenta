//! Ingestion helpers: extract text (per page), chunk it, and hash chunks for
//! idempotent re-ingest. Dumb fixed-size chunking for v1.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::core::{AgentaError, Result};

/// v1 chunk sizing (word-based ≈ token-based). Small chunks retrieve better.
pub const CHUNK_WORDS: usize = 350;
pub const CHUNK_OVERLAP: usize = 40;

pub struct Page {
    pub number: i32,
    pub text: String,
}

pub struct RawChunk {
    pub index: i32,
    pub text: String,
    pub page: i32,
}

/// Extract text per page. `.txt`/`.md` are a single page; `.pdf` is split per page
/// (via pdf-extract) so chunks keep their page number for citations.
pub fn extract_pages(path: &Path) -> Result<Vec<Page>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "txt" | "md" | "markdown" | "text" => {
            let text = std::fs::read_to_string(path)?;
            Ok(vec![Page { number: 1, text }])
        }
        "pdf" => {
            let pages = pdf_extract::extract_text_by_pages(path)
                .map_err(|e| AgentaError::Config(format!("PDF extraction failed: {}", e)))?;
            Ok(pages
                .into_iter()
                .enumerate()
                .map(|(i, text)| Page {
                    number: (i + 1) as i32,
                    text,
                })
                .collect())
        }
        other => Err(AgentaError::Config(format!(
            "Unsupported file type '.{}'. Use .pdf, .md, or .txt",
            other
        ))),
    }
}

/// Split each page into overlapping word windows, preserving page numbers. The
/// chunk index is a stable global counter so re-ingest produces identical ids.
pub fn chunk_pages(pages: &[Page]) -> Vec<RawChunk> {
    let step = CHUNK_WORDS.saturating_sub(CHUNK_OVERLAP).max(1);
    let mut chunks = Vec::new();
    let mut idx = 0i32;

    for page in pages {
        let words: Vec<&str> = page.text.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        let mut start = 0;
        while start < words.len() {
            let end = (start + CHUNK_WORDS).min(words.len());
            let text = words[start..end].join(" ");
            if !text.trim().is_empty() {
                chunks.push(RawChunk {
                    index: idx,
                    text,
                    page: page.number,
                });
                idx += 1;
            }
            if end == words.len() {
                break;
            }
            start += step;
        }
    }
    chunks
}

/// Stable content hash → idempotent chunk id. Same file re-ingests to the same ids.
pub fn chunk_id(source: &str, index: i32, text: &str) -> String {
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    index.hash(&mut h);
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// A short preview of extracted text so the user can eyeball extraction quality
/// (e.g. garbled Arabic) before embedding.
pub fn preview_sample(pages: &[Page], max_chars: usize) -> String {
    let joined: String = pages
        .iter()
        .find(|p| !p.text.trim().is_empty())
        .map(|p| p.text.clone())
        .unwrap_or_default();
    joined.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_is_deterministic_and_overlaps() {
        let pages = vec![Page {
            number: 1,
            text: (0..1000).map(|i| format!("w{}", i)).collect::<Vec<_>>().join(" "),
        }];
        let a = chunk_pages(&pages);
        let b = chunk_pages(&pages);
        assert_eq!(a.len(), b.len());
        assert!(a.len() > 1, "1000 words should yield multiple chunks");
        // same content → same id (idempotent)
        assert_eq!(
            chunk_id("f.md", a[0].index, &a[0].text),
            chunk_id("f.md", b[0].index, &b[0].text)
        );
        // different text → different id
        assert_ne!(
            chunk_id("f.md", a[0].index, &a[0].text),
            chunk_id("f.md", a[1].index, &a[1].text)
        );
    }
}
