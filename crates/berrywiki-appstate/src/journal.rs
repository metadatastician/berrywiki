//! A crash-recovery journal for multi-file operations (e.g. a subtree move).
//!
//! # Why not `git restore` / `reset --hard`
//!
//! The rejected design recovered by resetting the working tree to `HEAD`,
//! which would silently discard *any* uncommitted local edit — including work
//! unrelated to the failed operation. That violates "never lose local work".
//!
//! # The sound design
//!
//! An operation writes ALL its new files first, then deletes obsolete old
//! files. Because content is written before anything is deleted, at any crash
//! point either:
//!
//! * all new files exist (deletes may be partial) → **roll forward**: finish
//!   the pending deletes; or
//! * some new files are missing (so no delete has happened yet, since deletes
//!   run only after every write) → **roll back**: remove the partial new files;
//!   the old files are all still present.
//!
//! Recovery only ever touches the operation's *own* recorded files — never a
//! blanket working-tree reset — so unrelated local edits are never disturbed,
//! and no page's content can be lost.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// The recorded intent of a move-like operation: the new files it creates and
/// the old files it will delete once all writes succeed. Paths are relative to
/// the wiki root.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MoveJournal {
    pub new_paths: Vec<String>,
    pub old_paths: Vec<String>,
}

impl MoveJournal {
    /// Atomically write the journal (temp + rename), so it is never read
    /// half-written.
    pub fn write(&self, journal_path: &Path) -> io::Result<()> {
        if let Some(parent) = journal_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut body = String::new();
        for p in &self.new_paths {
            body.push_str("N ");
            body.push_str(p);
            body.push('\n');
        }
        for p in &self.old_paths {
            body.push_str("O ");
            body.push_str(p);
            body.push('\n');
        }
        let tmp = journal_path.with_extension("journal.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, journal_path)
    }

    /// Read the journal, or `None` if there is none.
    pub fn read(journal_path: &Path) -> io::Result<Option<MoveJournal>> {
        let text = match fs::read_to_string(journal_path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut j = MoveJournal::default();
        for line in text.lines() {
            if let Some(p) = line.strip_prefix("N ") {
                j.new_paths.push(p.to_string());
            } else if let Some(p) = line.strip_prefix("O ") {
                j.old_paths.push(p.to_string());
            }
        }
        Ok(Some(j))
    }

    /// Remove the journal (operation committed or recovered).
    pub fn clear(journal_path: &Path) -> io::Result<()> {
        match fs::remove_file(journal_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Recover an interrupted operation, if a journal exists. Returns whether a
/// recovery was performed. Never touches files outside the journal's own lists.
pub fn recover(journal_path: &Path, wiki_root: &Path) -> io::Result<bool> {
    let Some(j) = MoveJournal::read(journal_path)? else {
        return Ok(false);
    };
    let new_set: HashSet<&String> = j.new_paths.iter().collect();
    let all_new_present = j.new_paths.iter().all(|p| wiki_root.join(p).exists());

    if all_new_present {
        // Roll forward: the content is safely in the new files; finish deletes.
        for old in &j.old_paths {
            if !new_set.contains(old) {
                let _ = fs::remove_file(wiki_root.join(old));
            }
        }
    } else {
        // Roll back: writes were incomplete, so no delete has run yet and the
        // old files are intact. Remove the partial new files only.
        for new in &j.new_paths {
            if !j.old_paths.contains(new) {
                let _ = fs::remove_file(wiki_root.join(new));
            }
        }
    }
    MoveJournal::clear(journal_path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static C: AtomicUsize = AtomicUsize::new(0);

    fn scratch() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "bw-journal-{}-{}",
            std::process::id(),
            C.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn write_read_round_trip() {
        let dir = scratch();
        let jp = dir.join("op.journal");
        let j = MoveJournal {
            new_paths: vec!["A--x.md".into(), "A--y.md".into()],
            old_paths: vec!["x.md".into(), "y.md".into()],
        };
        j.write(&jp).unwrap();
        assert_eq!(MoveJournal::read(&jp).unwrap().unwrap(), j);
        MoveJournal::clear(&jp).unwrap();
        assert!(MoveJournal::read(&jp).unwrap().is_none());
    }

    #[test]
    fn roll_forward_completes_deletes_when_new_files_present() {
        let dir = scratch();
        let root = dir.join("wiki");
        fs::create_dir_all(&root).unwrap();
        // New files exist (content is safe); an old file still lingers.
        fs::write(root.join("A--x.md"), "content-x").unwrap();
        fs::write(root.join("x.md"), "content-x").unwrap(); // stale duplicate
        let jp = dir.join("op.journal");
        MoveJournal {
            new_paths: vec!["A--x.md".into()],
            old_paths: vec!["x.md".into()],
        }
        .write(&jp)
        .unwrap();

        assert!(recover(&jp, &root).unwrap());
        assert!(root.join("A--x.md").exists(), "new file kept");
        assert!(!root.join("x.md").exists(), "stale old file cleaned up");
        assert!(MoveJournal::read(&jp).unwrap().is_none(), "journal cleared");
    }

    #[test]
    fn roll_back_removes_partial_new_files_keeping_old() {
        let dir = scratch();
        let root = dir.join("wiki");
        fs::create_dir_all(&root).unwrap();
        // Writes were incomplete: only one of two new files got written; both
        // old files are still present (no delete had run).
        fs::write(root.join("A--x.md"), "content-x").unwrap(); // partial new
        fs::write(root.join("x.md"), "content-x").unwrap(); // old, intact
        fs::write(root.join("y.md"), "content-y").unwrap(); // old, intact
        let jp = dir.join("op.journal");
        MoveJournal {
            new_paths: vec!["A--x.md".into(), "A--y.md".into()], // A--y.md never written
            old_paths: vec!["x.md".into(), "y.md".into()],
        }
        .write(&jp)
        .unwrap();

        assert!(recover(&jp, &root).unwrap());
        // Partial new file removed; both old files (all content) preserved.
        assert!(!root.join("A--x.md").exists(), "partial new file rolled back");
        assert!(root.join("x.md").exists(), "old content preserved");
        assert!(root.join("y.md").exists(), "old content preserved");
    }

    #[test]
    fn recover_is_noop_without_a_journal() {
        let dir = scratch();
        assert!(!recover(&dir.join("none.journal"), &dir).unwrap());
    }
}
