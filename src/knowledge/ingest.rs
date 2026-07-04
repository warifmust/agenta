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

/// How to split extracted pages into chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkStrategy {
    /// Fixed overlapping word windows. Good default for prose.
    Words,
    /// One chunk per numbered entry, e.g. a supplication `(41)` in Hisnul Muslim,
    /// keeping the entry's section header attached so a topic word like "sujud"
    /// travels with its (otherwise Arabic-only) content.
    Entries,
}

impl ChunkStrategy {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "words" | "word" => Some(Self::Words),
            "entries" | "entry" => Some(Self::Entries),
            _ => None,
        }
    }
}

/// Dispatch to the chosen chunker.
pub fn chunk(pages: &[Page], strategy: ChunkStrategy) -> Vec<RawChunk> {
    match strategy {
        ChunkStrategy::Words => chunk_pages(pages),
        ChunkStrategy::Entries => chunk_by_entry(pages),
    }
}

fn push_entry(
    chunks: &mut Vec<RawChunk>,
    idx: &mut i32,
    header: &str,
    buf: &mut Vec<String>,
    page: i32,
) {
    if buf.is_empty() {
        return;
    }
    let body = buf.join(" ");
    buf.clear();
    if body.trim().is_empty() {
        return;
    }
    let text = if header.trim().is_empty() {
        body
    } else {
        format!("{} {}", header.trim(), body)
    };
    chunks.push(RawChunk { index: *idx, text, page });
    *idx += 1;
}

/// A section header sitting right before a marker, e.g. "19. Whilst prostrating
/// [sujud]" — number, dot, then a capitalised word. Used to peel a header off the
/// tail of one entry's text so it can be attached to the NEXT entry.
fn split_trailing_header(text: &str) -> (String, Option<String>) {
    let header_re = regex::Regex::new(r"\b\d{1,3}\.\s+\p{Lu}").unwrap();
    // The header, if any, is the LAST such match and hugs the following marker, so
    // the text from it to the segment end is a SHORT title (not a paragraph). This
    // length cap is what distinguishes a real header from a stray "3. Word" mid-text.
    const MAX_HEADER_LEN: usize = 80;
    if let Some(m) = header_re.find_iter(text).last() {
        let header = text[m.start()..].trim();
        if header.len() <= MAX_HEADER_LEN {
            let body = text[..m.start()].trim().to_string();
            return (body, Some(header.to_string()));
        }
    }
    (text.trim().to_string(), None)
}

/// Structure-aware chunker for numbered reference texts (Hisnul Muslim et al.).
/// Splits the (continuous, OCR-style) text at every `(N)` entry marker and attaches
/// the running `NN. Section header`, so each chunk is one self-contained,
/// topically-labelled supplication. Falls back to word chunking if the document
/// has too few entry markers to be structured.
pub fn chunk_by_entry(pages: &[Page]) -> Vec<RawChunk> {
    let entry_re = regex::Regex::new(r"\(\d{1,4}\)").unwrap();

    // Concatenate all pages, tracking the source page for every byte offset so an
    // entry can be cited to the page its marker appears on.
    let mut full = String::new();
    let mut page_at: Vec<i32> = Vec::new();
    for p in pages {
        for _ in 0..p.text.len() {
            page_at.push(p.number);
        }
        full.push_str(&p.text);
        // separator between pages
        full.push(' ');
        page_at.push(p.number);
    }

    let markers: Vec<(usize, usize)> =
        entry_re.find_iter(&full).map(|m| (m.start(), m.end())).collect();
    if markers.len() < 3 {
        return chunk_pages(pages); // not a structured doc
    }

    let mut chunks: Vec<RawChunk> = Vec::new();
    let mut idx = 0i32;
    let mut current_header = String::new();
    let mut pending_header: Option<String> = None;

    // A header may precede the very first marker (in the preamble) — capture it.
    if let (_, Some(h)) = split_trailing_header(&full[..markers[0].0]) {
        current_header = h;
    }

    for i in 0..markers.len() {
        // Adopt the header peeled off the previous entry's tail.
        if let Some(h) = pending_header.take() {
            current_header = h;
        }
        let (mstart, mend) = markers[i];
        let seg_end = markers.get(i + 1).map(|m| m.0).unwrap_or(full.len());
        let marker = full[mstart..mend].trim().to_string();
        let raw_body = &full[mend..seg_end];

        // Peel any section header hugging the next marker off the tail.
        let (body, trailing) = split_trailing_header(raw_body);
        pending_header = trailing;

        let page = page_at.get(mstart).copied().unwrap_or(1);
        let combined = if current_header.is_empty() {
            format!("{} {}", marker, body)
        } else {
            format!("{} {} {}", current_header, marker, body)
        };
        let mut buf = vec![combined];
        push_entry(&mut chunks, &mut idx, "", &mut buf, page);
    }
    chunks
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
    fn entry_chunker_attaches_header_to_content() {
        // Mimics the real Hisnul Muslim layout: the section header + first entry
        // marker close one page, the actual dhikr opens the next.
        let pages = vec![
            Page {
                number: 42,
                text: "and majesty.' 19. Whilst prostrating [sujud] (41)".to_string(),
            },
            Page {
                number: 43,
                text: "How Perfect my Lord is, The Most High. [three times] (42) \
                       How perfect You are O Allah, forgive me. (43) \
                       O Allah unto You I have prostrated."
                    .to_string(),
            },
            Page {
                number: 45,
                text: "20. Between the two prostrations (48) My Lord forgive me. \
                       (49) O Allah forgive me and have mercy."
                    .to_string(),
            },
        ];
        let chunks = chunk_by_entry(&pages);
        // 5 numbered entries → 5 chunks (41,42,43,48,49).
        assert_eq!(chunks.len(), 5, "one chunk per entry");

        // The whole point: the entry holding "How Perfect my Lord is" must ALSO
        // carry its "Whilst prostrating [sujud]" header, so a sujud query matches.
        let sujud_entry = chunks
            .iter()
            .find(|c| c.text.contains("How Perfect my Lord is"))
            .expect("entry 41 present");
        assert!(
            sujud_entry.text.contains("Whilst prostrating [sujud]"),
            "header must be attached to the dhikr: {:?}",
            sujud_entry.text
        );
        assert_eq!(sujud_entry.page, 42, "cites the page the entry began on");

        // Entries under section 20 carry that header, not section 19's.
        let between = chunks
            .iter()
            .find(|c| c.text.contains("My Lord forgive me"))
            .expect("entry 48 present");
        assert!(between.text.contains("Between the two prostrations"));
        assert!(!between.text.contains("prostrating [sujud]"));
    }

    #[test]
    fn entry_chunker_falls_back_when_unstructured() {
        // Prose with no entry markers → must not shatter; falls back to words.
        let pages = vec![Page {
            number: 1,
            text: (0..500).map(|i| format!("w{}", i)).collect::<Vec<_>>().join(" "),
        }];
        let entry = chunk_by_entry(&pages);
        let words = chunk_pages(&pages);
        assert_eq!(entry.len(), words.len(), "no markers → word chunking");
    }

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
