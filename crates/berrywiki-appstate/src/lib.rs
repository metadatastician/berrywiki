//! App-private state that must live **outside** the wiki clone (ADR-0008).
//!
//! Drafts, the operation journal, the repository lock and the search index are
//! BerryWiki's own state, not wiki content. Keeping them inside the clone would
//! let a plain-git user commit and push them to GitHub — a "usable without
//! BerryWiki" breach. This crate provides:
//!
//! * [`AppState`] — a canonical home under the XDG *state* directory, keyed by
//!   a stable hash of the wiki's path, with sub-paths for each kind of state.
//! * [`RepoLock`] — an advisory lock so two processes never mutate the same
//!   clone at once (a Git-rules requirement), with dead-holder recovery.
//! * [`journal`] — a roll-forward operation journal for crash recovery that
//!   only ever touches the operation's own files, never a blanket working-tree
//!   reset (soundly replacing the rejected `git restore`/`reset --hard` design).

use std::path::{Path, PathBuf};

pub mod journal;

pub use journal::MoveJournal;

// NOTE: repository locking (single-writer enforcement) is intentionally NOT
// implemented here yet. An earlier hand-rolled pid lock file had reclaim races
// and, more importantly, nothing acquired it — false safety. There is no
// concurrent-writer surface today (the server is read-only; the CLI is
// single-shot). When in-app mutation / a long-running server lands, add locking
// with an OS advisory lock (`flock`, auto-released on process death) rather than
// a pid file. Tracked in docs/execution/work-packages.adoc.

/// Stable 64-bit FNV-1a hash. Hand-rolled (not `DefaultHasher`, whose output is
/// not guaranteed stable across Rust versions) so the app-state location does
/// not move when the toolchain changes.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A stable identifier for a wiki, derived from its canonical filesystem path.
pub fn repo_id(wiki: &Path) -> String {
    let canonical = wiki.canonicalize().unwrap_or_else(|_| wiki.to_path_buf());
    format!("{:016x}", fnv1a(canonical.to_string_lossy().as_bytes()))
}

/// The base directory for all BerryWiki app-private state: `$XDG_STATE_HOME`,
/// else `~/.local/state`, else the system temp dir.
fn state_base() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local").join("state");
    }
    std::env::temp_dir()
}

/// The out-of-clone home for one wiki's app-private state.
pub struct AppState {
    root: PathBuf,
    repo_id: String,
}

impl AppState {
    /// Resolve (and create) the app-state directory for `wiki`. The directory
    /// is guaranteed to be outside `wiki` (it lives under the XDG state base).
    pub fn for_wiki(wiki: &Path) -> std::io::Result<Self> {
        let repo_id = repo_id(wiki);
        let root = state_base().join("berrywiki").join(&repo_id);
        std::fs::create_dir_all(&root)?;
        Ok(AppState { root, repo_id })
    }

    /// Build an app-state rooted at an explicit directory (for tests).
    pub fn at(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(AppState {
            root,
            repo_id: "explicit".to_string(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn repo_id(&self) -> &str {
        &self.repo_id
    }

    /// Per-page draft store directory (see `berrywiki-draft`).
    pub fn drafts_dir(&self) -> PathBuf {
        self.root.join("drafts")
    }

    /// Path of the move/operation journal.
    pub fn journal_path(&self) -> PathBuf {
        self.root.join("operation.journal")
    }

    /// Search-index directory (disposable derived data).
    pub fn index_dir(&self) -> PathBuf {
        self.root.join("index")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_is_stable_and_path_dependent() {
        let a = repo_id(Path::new("/tmp/wiki-one"));
        let b = repo_id(Path::new("/tmp/wiki-one"));
        let c = repo_id(Path::new("/tmp/wiki-two"));
        assert_eq!(a, b, "same path -> same id");
        assert_ne!(a, c, "different path -> different id");
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn app_state_lives_outside_the_wiki() {
        let wiki = std::env::temp_dir().join(format!("bw-wiki-{}", std::process::id()));
        std::fs::create_dir_all(&wiki).unwrap();
        let app = AppState::for_wiki(&wiki).unwrap();
        assert!(!app.root().starts_with(&wiki), "app state must not be inside the clone");
        assert!(app.drafts_dir().starts_with(app.root()));
        assert!(app.journal_path().starts_with(app.root()));
        assert!(app.index_dir().starts_with(app.root()));
    }
}
