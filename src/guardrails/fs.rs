//! Filesystem access policy — the ONE place a path decision is made.
//!
//! Centralised so the subtle parts (canonicalisation, `..` traversal, symlink
//! escapes, the sensitive denylist, the per-agent allowlist) live in one tested
//! unit instead of being re-implemented — and inevitably drifting — across
//! `read_file`, `list_files`, and `write_file`.
//!
//! Model (deny-except-roots, with a hard sensitive floor):
//!   1. Resolve the requested path to a real absolute path (symlinks + `..`
//!      collapsed) so string tricks can't dodge the checks.
//!   2. If it hits the sensitive denylist AND isn't an *exact* explicit grant →
//!      DENY. The floor wins over a broad allowed root: allowing `~` does not
//!      expose `~/.ssh`.
//!   3. Else if it's under one of the agent's allowed roots → ALLOW.
//!   4. Else → DENY (outside allowed paths).
//!
//! An agent's "allowed roots" are its `fs_allow` list (task agents) or its trusted
//! directories (MIND). Empty roots ⇒ no filesystem access at all, which is the
//! safe default.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsMode {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsDecision {
    Allow,
    /// Human-readable reason, safe to show the agent/user (never echoes secrets).
    Deny(String),
}

impl FsDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, FsDecision::Allow)
    }
}

/// Sensitive directories directly under `$HOME` — anything beneath `~/<name>` is
/// denied. Credential and key stores whose leak would be catastrophic.
const SENSITIVE_HOME_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".gpg",
    ".kube",
    ".docker",
    ".config/gcloud",
    ".config/gh",
    ".agenta", // agenta's own brain: .env with every key + bot token, the DB
];

/// Sensitive files anywhere, matched on the final component. Exact names plus the
/// `.env` family (`.env`, `.env.local`, …). A specific file can still be granted by
/// listing its exact path in the agent's allowed roots (see `is_exact_grant`).
const SENSITIVE_FILE_NAMES: &[&str] = &[
    ".netrc", ".npmrc", ".pypirc", "credentials", "id_rsa", "id_dsa", "id_ecdsa",
    "id_ed25519",
];

fn is_sensitive_filename(name: &str) -> bool {
    SENSITIVE_FILE_NAMES.contains(&name) || name == ".env" || name.starts_with(".env.")
}

/// Expand a leading `~` against `home`. Anything else is returned unchanged.
fn expand_home(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(raw)
}

/// Resolve `raw` to a real absolute path: expand `~`, make absolute against `base`,
/// then collapse symlinks and `..` on the part that exists and fold the rest
/// lexically. The lexical tail can't traverse a symlink (it doesn't exist yet), so
/// `..`-popping there is safe.
pub fn resolve_path(raw: &str, base: &Path, home: &Path) -> PathBuf {
    let expanded = expand_home(raw, home);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };

    // Fast path: the whole thing exists → let the OS resolve it fully.
    if let Ok(real) = absolute.canonicalize() {
        return real;
    }

    // Otherwise walk components, canonicalising the longest existing prefix (which
    // resolves real symlinks and `..` up to there) and applying the rest lexically.
    let mut real = PathBuf::new();
    for comp in absolute.components() {
        match comp {
            Component::Prefix(p) => real.push(p.as_os_str()),
            Component::RootDir => real.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop the last real component. If `real` so far exists and is a
                // symlink, canonicalise before popping so `..` acts on the target.
                if let Ok(c) = real.canonicalize() {
                    real = c;
                }
                real.pop();
            }
            Component::Normal(name) => {
                real.push(name);
                // Resolve as far as the fs actually goes, one existing step at a
                // time, so a symlinked ancestor can't hide a sensitive target.
                if let Ok(c) = real.canonicalize() {
                    real = c;
                }
            }
        }
    }
    real
}

/// If `p` is sensitive, the human-readable name of what it is; else None.
fn sensitive_reason(p: &Path, home: &Path) -> Option<String> {
    for dir in SENSITIVE_HOME_DIRS {
        let root = home.join(dir);
        if p == root || p.starts_with(&root) {
            return Some(format!("~/{dir}"));
        }
    }
    if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
        if is_sensitive_filename(name) {
            return Some(name.to_string());
        }
    }
    None
}

/// The core decision. `roots` are the agent's allowed paths (already raw strings
/// from config/trust store — resolved here). `base` is the directory relative paths
/// resolve against (the daemon cwd); `home` is `$HOME`.
pub fn check_fs_access(
    roots: &[String],
    raw_path: &str,
    base: &Path,
    home: &Path,
    _mode: FsMode,
) -> FsDecision {
    // Canonicalise home/base first so every comparison happens in one namespace.
    // Without this, `canonicalize()` on the target resolves OS-level symlinks (on
    // macOS /var -> /private/var, on some setups a symlinked $HOME) while the
    // denylist/roots would still be phrased in the un-resolved form — and a
    // sensitive path would slip through the mismatch. Caught by the test matrix.
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());

    let target = resolve_path(raw_path, &base, &home);
    let resolved_roots: Vec<PathBuf> =
        roots.iter().map(|r| resolve_path(r, &base, &home)).collect();

    // Sensitive floor — overridable only by an EXACT explicit grant of that path,
    // not by a broad parent root.
    if let Some(what) = sensitive_reason(&target, &home) {
        let exact_grant = resolved_roots.iter().any(|r| *r == target);
        if !exact_grant {
            return FsDecision::Deny(format!(
                "protected path ({what}) — denied for safety; grant it explicitly if this agent truly needs it"
            ));
        }
    }

    if resolved_roots.iter().any(|r| target == *r || target.starts_with(r)) {
        return FsDecision::Allow;
    }

    FsDecision::Deny(format!(
        "{} is outside this agent's allowed paths",
        target.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn home() -> PathBuf {
        // A real temp dir so canonicalize() works; each test seeds what it needs.
        std::env::temp_dir().join(format!("agfs_{}", std::process::id()))
    }

    fn setup() -> PathBuf {
        let h = home();
        let _ = fs::create_dir_all(h.join(".ssh"));
        let _ = fs::write(h.join(".ssh").join("id_rsa"), "KEY");
        let _ = fs::create_dir_all(h.join("Works/proj"));
        let _ = fs::write(h.join("Works/proj/main.rs"), "code");
        let _ = fs::write(h.join("Works/proj/.env"), "SECRET=1");
        h
    }

    #[test]
    fn allows_within_a_granted_root() {
        let h = setup();
        let roots = vec![h.join("Works/proj").to_string_lossy().into_owned()];
        let d = check_fs_access(&roots, &h.join("Works/proj/main.rs").to_string_lossy(), &h, &h, FsMode::Read);
        assert_eq!(d, FsDecision::Allow);
    }

    #[test]
    fn denies_outside_all_roots() {
        let h = setup();
        let roots = vec![h.join("Works/proj").to_string_lossy().into_owned()];
        let d = check_fs_access(&roots, &h.join("Works/other/x").to_string_lossy(), &h, &h, FsMode::Read);
        assert!(matches!(d, FsDecision::Deny(_)));
    }

    #[test]
    fn empty_roots_deny_everything() {
        let h = setup();
        let d = check_fs_access(&[], &h.join("Works/proj/main.rs").to_string_lossy(), &h, &h, FsMode::Read);
        assert!(matches!(d, FsDecision::Deny(_)));
    }

    #[test]
    fn sensitive_dir_denied_even_when_a_parent_is_granted() {
        let h = setup();
        // Grant the whole home — the floor must STILL block ~/.ssh.
        let roots = vec![h.to_string_lossy().into_owned()];
        let d = check_fs_access(&roots, &h.join(".ssh/id_rsa").to_string_lossy(), &h, &h, FsMode::Read);
        assert!(matches!(d, FsDecision::Deny(ref r) if r.contains(".ssh")));
    }

    #[test]
    fn dotenv_denied_by_default_but_allowed_by_exact_grant() {
        let h = setup();
        let env_path = h.join("Works/proj/.env").to_string_lossy().into_owned();
        // Broad grant of the repo: .env still denied.
        let broad = vec![h.join("Works/proj").to_string_lossy().into_owned()];
        assert!(matches!(check_fs_access(&broad, &env_path, &h, &h, FsMode::Read), FsDecision::Deny(_)));
        // Exact grant of the .env: allowed.
        let exact = vec![env_path.clone()];
        assert_eq!(check_fs_access(&exact, &env_path, &h, &h, FsMode::Read), FsDecision::Allow);
    }

    #[test]
    fn dotdot_traversal_cannot_escape_a_root_into_a_sensitive_path() {
        let h = setup();
        let roots = vec![h.join("Works/proj").to_string_lossy().into_owned()];
        // proj/../../.ssh/id_rsa resolves to ~/.ssh/id_rsa — must be denied.
        let sneaky = h.join("Works/proj/../../.ssh/id_rsa").to_string_lossy().into_owned();
        let d = check_fs_access(&roots, &sneaky, &h, &h, FsMode::Read);
        assert!(matches!(d, FsDecision::Deny(_)), "traversal into ~/.ssh must be denied, got {d:?}");
    }

    #[test]
    fn symlink_out_of_a_root_is_resolved_and_denied() {
        let h = setup();
        // proj/link -> ~/.ssh ; reading proj/link/id_rsa must resolve to ~/.ssh.
        let link = h.join("Works/proj/link");
        let _ = fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(h.join(".ssh"), &link).unwrap();
        let roots = vec![h.join("Works/proj").to_string_lossy().into_owned()];
        let d = check_fs_access(&roots, &link.join("id_rsa").to_string_lossy(), &h, &h, FsMode::Read);
        assert!(matches!(d, FsDecision::Deny(ref r) if r.contains(".ssh")), "symlink escape must be denied, got {d:?}");
    }

    #[test]
    fn tilde_and_relative_paths_resolve() {
        let h = setup();
        let roots = vec!["~/Works/proj".to_string()];
        // ~ form
        assert_eq!(check_fs_access(&roots, "~/Works/proj/main.rs", &h, &h, FsMode::Read), FsDecision::Allow);
        // relative form, base = the repo
        let base = h.join("Works/proj");
        assert_eq!(check_fs_access(&roots, "main.rs", &base, &h, FsMode::Read), FsDecision::Allow);
    }
}
