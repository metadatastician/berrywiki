//! A crash-recovery journal for multi-file operations (the subtree move).
//!
//! # Why not `git restore` / `reset --hard`
//!
//! The rejected design recovered by resetting the working tree to `HEAD`,
//! discarding *any* uncommitted edit — including work unrelated to the failed
//! operation. That violates "never lose local work".
//!
//! # What this module stores
//!
//! Just the *data*: for each renamed file, its old path, new path, and the
//! old file's fingerprint (`mtime`, length) captured before the operation
//! began. Recovery *policy* lives in the store (it needs the link-rewriting
//! logic), and uses these fingerprints so it never deletes an old file that
//! was edited on disk after the crash (a "wiki without the app" edit).
//!
//! An operation writes ALL its new files first, then deletes old files, so at
//! any crash point either all new files exist (content is safe → the store
//! rolls forward, deleting only *unchanged* old files) or some are missing (no
//! delete has run → the store rolls back the partial new files). Recovery only
//! ever touches the operation's own recorded files, never a working-tree reset.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// One recorded rename: old → new, plus the old file's pre-operation
/// fingerprint so recovery can tell a stale leftover from a fresh edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameEntry {
    pub old_path: String,
    pub new_path: String,
    pub old_mtime: u128,
    pub old_len: u64,
}

/// The recorded intent of a move-like operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MoveJournal {
    pub entries: Vec<RenameEntry>,
}

impl MoveJournal {
    /// Atomically write the journal (temp + fsync + rename).
    pub fn write(&self, journal_path: &Path) -> io::Result<()> {
        if let Some(parent) = journal_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut body = String::new();
        for e in &self.entries {
            // "<mtime> <len> <old_path>\t<new_path>". Filenames never contain a
            // tab or newline (validated), so this parses unambiguously even if a
            // path contained spaces.
            body.push_str(&format!(
                "{} {} {}\t{}\n",
                e.old_mtime, e.old_len, e.old_path, e.new_path
            ));
        }
        let tmp = journal_path.with_extension("journal.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, journal_path)?;
        fsync_parent(journal_path);
        Ok(())
    }

    /// Read the journal, or `None` if there is none.
    pub fn read(journal_path: &Path) -> io::Result<Option<MoveJournal>> {
        let text = match fs::read_to_string(journal_path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut entries = Vec::new();
        for line in text.lines() {
            if let Some(entry) = parse_line(line) {
                entries.push(entry);
            }
        }
        Ok(Some(MoveJournal { entries }))
    }

    /// Remove the journal durably (also fsync the directory, so a successful
    /// operation's cleared journal does not resurrect after a power loss).
    pub fn clear(journal_path: &Path) -> io::Result<()> {
        match fs::remove_file(journal_path) {
            Ok(()) => {
                fsync_parent(journal_path);
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

fn parse_line(line: &str) -> Option<RenameEntry> {
    let (left, new_path) = line.split_once('\t')?;
    let mut parts = left.splitn(3, ' ');
    let old_mtime = parts.next()?.parse().ok()?;
    let old_len = parts.next()?.parse().ok()?;
    let old_path = parts.next()?.to_string();
    if old_path.is_empty() || new_path.is_empty() {
        return None;
    }
    Some(RenameEntry {
        old_path,
        new_path: new_path.to_string(),
        old_mtime,
        old_len,
    })
}

/// Best-effort directory fsync so a rename/remove is durable. Unsupported on
/// some platforms/filesystems; a failure is not fatal.
fn fsync_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static C: AtomicUsize = AtomicUsize::new(0);

    fn jp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "bw-journal-{}-{}",
            std::process::id(),
            C.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&d).unwrap();
        d.join("op.journal")
    }

    #[test]
    fn write_read_round_trip_with_fingerprints() {
        let p = jp();
        let j = MoveJournal {
            entries: vec![
                RenameEntry {
                    old_path: "Plan.md".into(),
                    new_path: "Team--Plan.md".into(),
                    old_mtime: 123456789,
                    old_len: 42,
                },
                RenameEntry {
                    old_path: "Plan--Notes.md".into(),
                    new_path: "Team--Plan--Notes.md".into(),
                    old_mtime: 987654321,
                    old_len: 7,
                },
            ],
        };
        j.write(&p).unwrap();
        assert_eq!(MoveJournal::read(&p).unwrap().unwrap(), j);
    }

    #[test]
    fn clear_removes_and_read_is_none() {
        let p = jp();
        MoveJournal {
            entries: vec![RenameEntry {
                old_path: "a.md".into(),
                new_path: "b.md".into(),
                old_mtime: 1,
                old_len: 1,
            }],
        }
        .write(&p)
        .unwrap();
        MoveJournal::clear(&p).unwrap();
        assert!(MoveJournal::read(&p).unwrap().is_none());
        MoveJournal::clear(&p).unwrap(); // idempotent
    }

    #[test]
    fn missing_journal_reads_none() {
        assert!(MoveJournal::read(&jp()).unwrap().is_none());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let p = jp();
        fs::write(&p, "garbage without a tab\n1 2 ok.md\tnew.md\n").unwrap();
        let j = MoveJournal::read(&p).unwrap().unwrap();
        assert_eq!(j.entries.len(), 1);
        assert_eq!(j.entries[0].old_path, "ok.md");
    }
}
