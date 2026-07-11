//! Persistent, per-page draft store (ADR-0006).
//!
//! A zero-JavaScript SSR app cannot detect idle or auto-submit, so "never lose
//! a keystroke" via *silent* autosave is impossible. BerryWiki instead offers
//! **explicit Save and Save-draft** actions; a saved draft is persisted here,
//! keyed by page id, **outside the wiki clone** (so it can never be committed
//! or pushed to GitHub, and it survives a process kill). This module is that
//! store: opaque text in, opaque text out, atomic writes.
//!
//! It is deliberately decoupled from the engine — a draft is just the pending
//! Markdown source for a page id.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub type Result<T> = std::result::Result<T, DraftError>;

#[derive(Debug)]
pub enum DraftError {
    InvalidPageId(String),
    Io { context: String, source: std::io::Error },
}

impl DraftError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        DraftError::Io {
            context: context.into(),
            source,
        }
    }
}

impl std::fmt::Display for DraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DraftError::InvalidPageId(id) => write!(f, "Invalid page id for a draft: {id:?}."),
            DraftError::Io { context, source } => write!(f, "Draft I/O ({context}): {source}."),
        }
    }
}

impl std::error::Error for DraftError {}

/// A loaded draft: the pending source plus when it was last saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub page_id: String,
    pub content: String,
    /// Last-saved time, from the draft file's mtime. `None` if unavailable.
    pub saved_at: Option<SystemTime>,
}

/// A listing entry (without loading the content).
#[derive(Debug, Clone)]
pub struct DraftSummary {
    pub page_id: String,
    pub saved_at: Option<SystemTime>,
}

const EXT: &str = "draft";

/// A directory of drafts. `root` must be OUTSIDE any wiki clone — typically an
/// XDG-cache path keyed by repo identity (the caller chooses it).
pub struct DraftStore {
    root: PathBuf,
}

impl DraftStore {
    /// Open (creating on first write) a draft store rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DraftStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Map a page id to its draft filename, rejecting anything that could
    /// escape `root`. Page ids are BerryWiki-generated (UUIDs / short slugs);
    /// non-`[A-Za-z0-9._-]` characters are replaced, and traversal is refused.
    fn file_for(&self, page_id: &str) -> Result<PathBuf> {
        if page_id.is_empty() || page_id.len() > 200 {
            return Err(DraftError::InvalidPageId(page_id.to_string()));
        }
        if page_id.contains('/') || page_id.contains('\\') || page_id.contains("..") {
            return Err(DraftError::InvalidPageId(page_id.to_string()));
        }
        let safe: String = page_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' })
            .collect();
        if safe.starts_with('.') {
            return Err(DraftError::InvalidPageId(page_id.to_string()));
        }
        Ok(self.root.join(format!("{safe}.{EXT}")))
    }

    /// Persist a draft for `page_id` (explicit Save-draft). Atomic write.
    pub fn save(&self, page_id: &str, content: &str) -> Result<()> {
        let path = self.file_for(page_id)?;
        fs::create_dir_all(&self.root)
            .map_err(|e| DraftError::io("creating the draft directory", e))?;
        let tmp = path.with_extension(format!("{EXT}.tmp-{}", std::process::id()));
        {
            let mut f = fs::File::create(&tmp)
                .map_err(|e| DraftError::io("creating a temp draft file", e))?;
            f.write_all(content.as_bytes())
                .map_err(|e| DraftError::io("writing a draft", e))?;
            f.sync_all()
                .map_err(|e| DraftError::io("syncing a draft", e))?;
        }
        fs::rename(&tmp, &path).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            DraftError::io("renaming a draft into place", e)
        })?;
        Ok(())
    }

    /// Load the draft for `page_id`, if one exists.
    pub fn load(&self, page_id: &str) -> Result<Option<Draft>> {
        let path = self.file_for(page_id)?;
        match fs::read_to_string(&path) {
            Ok(content) => {
                let saved_at = fs::metadata(&path).and_then(|m| m.modified()).ok();
                Ok(Some(Draft {
                    page_id: page_id.to_string(),
                    content,
                    saved_at,
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(DraftError::io(format!("reading the draft for {page_id:?}"), e)),
        }
    }

    /// True if a draft exists for `page_id`.
    pub fn has(&self, page_id: &str) -> bool {
        self.file_for(page_id).map(|p| p.exists()).unwrap_or(false)
    }

    /// Delete the draft for `page_id` (e.g. after a successful Save/commit).
    /// Succeeds even if there was no draft.
    pub fn discard(&self, page_id: &str) -> Result<()> {
        let path = self.file_for(page_id)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DraftError::io(format!("discarding the draft for {page_id:?}"), e)),
        }
    }

    /// List all drafts (page id + saved time), deterministically ordered.
    pub fn list(&self) -> Vec<DraftSummary> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(EXT) {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(DraftSummary {
                    page_id: stem.to_string(),
                    saved_at: entry.metadata().and_then(|m| m.modified()).ok(),
                });
            }
        }
        out.sort_by(|a, b| a.page_id.cmp(&b.page_id));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static C: AtomicUsize = AtomicUsize::new(0);

    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!(
            "berrywiki-draft-{}-{}",
            std::process::id(),
            C.fetch_add(1, Ordering::SeqCst)
        ))
    }

    #[test]
    fn save_load_round_trip() {
        let s = DraftStore::new(scratch());
        assert!(s.load("page-1").unwrap().is_none());
        s.save("page-1", "# WIP\n\nhalf a thought").unwrap();
        let d = s.load("page-1").unwrap().unwrap();
        assert_eq!(d.content, "# WIP\n\nhalf a thought");
        assert_eq!(d.page_id, "page-1");
        assert!(s.has("page-1"));
    }

    #[test]
    fn save_overwrites() {
        let s = DraftStore::new(scratch());
        s.save("p", "first").unwrap();
        s.save("p", "second").unwrap();
        assert_eq!(s.load("p").unwrap().unwrap().content, "second");
    }

    #[test]
    fn draft_survives_a_new_store_over_same_dir() {
        // Simulates "an unsaved draft survives a killed process": persistence is
        // to disk, so a fresh store instance still sees it.
        let dir = scratch();
        DraftStore::new(&dir).save("keep", "precious").unwrap();
        let reopened = DraftStore::new(&dir);
        assert_eq!(reopened.load("keep").unwrap().unwrap().content, "precious");
    }

    #[test]
    fn discard_removes_and_is_idempotent() {
        let s = DraftStore::new(scratch());
        s.save("x", "y").unwrap();
        s.discard("x").unwrap();
        assert!(!s.has("x"));
        s.discard("x").unwrap(); // no error when already gone
    }

    #[test]
    fn list_is_sorted() {
        let s = DraftStore::new(scratch());
        s.save("bbb", "1").unwrap();
        s.save("aaa", "2").unwrap();
        let ids: Vec<String> = s.list().into_iter().map(|d| d.page_id).collect();
        assert_eq!(ids, ["aaa", "bbb"]);
    }

    #[test]
    fn rejects_traversal_page_ids() {
        let s = DraftStore::new(scratch());
        assert!(matches!(s.save("../evil", "x"), Err(DraftError::InvalidPageId(_))));
        assert!(matches!(s.save("a/b", "x"), Err(DraftError::InvalidPageId(_))));
        assert!(matches!(s.save("", "x"), Err(DraftError::InvalidPageId(_))));
    }

    #[test]
    fn uuid_page_ids_work() {
        let s = DraftStore::new(scratch());
        let id = "0195f6ec-36a2-7a42-b519-5f558842e256";
        s.save(id, "draft body").unwrap();
        assert_eq!(s.load(id).unwrap().unwrap().content, "draft body");
    }
}
