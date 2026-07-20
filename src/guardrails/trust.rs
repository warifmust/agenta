//! MIND's trusted-directory store.
//!
//! MIND is the interactive builder, so — unlike a task agent's static `fs_allow` —
//! it earns filesystem access by being *trusted* into a directory: the chat asks
//! "Trust this directory?" the first time it runs from a new path, and approved
//! paths are remembered here so future runs under them just work. The daemon reads
//! this store to know MIND's allowed roots; the CLI writes it at chat startup.
//!
//! The sensitive floor (see [`super::fs`]) still applies inside a trusted directory
//! — trusting `~` does not expose `~/.ssh`.

use std::path::{Path, PathBuf};

/// `~/.agenta/trusted_dirs` — one absolute path per line. Lives under `.agenta`,
/// which agents themselves can't read (it's on the sensitive floor); the CLI/daemon
/// read and write it directly, not through the guarded file tools.
fn store_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".agenta").join("trusted_dirs"))
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// The trusted directories (absolute paths). Missing store ⇒ empty ⇒ MIND has no
/// filesystem access until a directory is trusted — the safe default.
pub fn load() -> Vec<String> {
    let Some(p) = store_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(&p)
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Is `dir` itself, or a directory that contains it, already trusted?
pub fn is_trusted(dir: &Path, trusted: &[String]) -> bool {
    let d = canonical(dir);
    trusted.iter().any(|t| {
        let tp = canonical(Path::new(t));
        d == tp || d.starts_with(&tp)
    })
}

/// Remember `dir` as trusted (canonicalised, deduped). Best-effort — a failure to
/// persist just means MIND will ask again next time, never a crash.
pub fn add(dir: &Path) {
    let Some(p) = store_path() else {
        return;
    };
    let entry = canonical(dir).to_string_lossy().to_string();
    let mut current = load();
    if current.iter().any(|t| *t == entry) {
        return;
    }
    current.push(entry);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, current.join("\n") + "\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_trusted_matches_the_dir_and_its_descendants() {
        let base = std::env::temp_dir();
        let trusted = vec![base.join("Works").to_string_lossy().into_owned()];
        // exact
        assert!(is_trusted(&base.join("Works"), &trusted));
        // descendant
        assert!(is_trusted(&base.join("Works/proj/src"), &trusted));
        // sibling — not trusted
        assert!(!is_trusted(&base.join("Other"), &trusted));
        // empty store trusts nothing
        assert!(!is_trusted(&base.join("Works"), &[]));
    }
}
