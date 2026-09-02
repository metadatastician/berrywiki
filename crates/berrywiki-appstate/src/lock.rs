// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Single-writer repository lock (ADR-0008, work package X-lock).
//!
//! One wiki clone has at most one BerryWiki *writer* at a time: a
//! `berrywiki serve` process for its whole lifetime, or a CLI mutation such as
//! `sidebar --write` for its duration. The lock is an OS advisory lock on
//! `<app-state>/lock`, taken with [`File::try_lock`] (`flock` on Unix,
//! `LockFileEx` on Windows; stable since Rust 1.89). The operating system
//! releases it when the holding process exits, however it exits, so there is
//! no stale lock and nothing to reclaim.
//!
//! The lock file's *contents* (`pid=… program=… since=…`) are a diagnostic,
//! written by the holder after acquisition so a refused writer can say who has
//! the wiki. They are never consulted to decide whether the lock is free; the
//! kernel decides that. A pid-file design that did consult them was removed
//! for reclaim races (ADR-0008), and this module must not grow one back.
//!
//! The lock lives in app state, outside the clone, so it never reaches git.

use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::AppState;

/// Why the writer lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another writer holds the lock. `holder` is the diagnostic record it
    /// wrote (`unknown` when it cannot be read), `path` is the lock file.
    Held { holder: String, path: PathBuf },
    /// The lock file could not be opened or locked for a reason other than
    /// contention.
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Held { holder, path } => write!(
                f,
                "wiki is already in use by {holder} (lock file {})",
                path.display()
            ),
            LockError::Io(e) => write!(f, "cannot take the writer lock: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

impl From<io::Error> for LockError {
    fn from(e: io::Error) -> Self {
        LockError::Io(e)
    }
}

/// An exclusive writer lock on one wiki's app state. Released when dropped,
/// and by the OS if the process dies first.
#[derive(Debug)]
pub struct RepoLock {
    file: File,
    path: PathBuf,
}

impl RepoLock {
    /// Try to become the wiki's single writer. `label` names the caller in the
    /// diagnostic record (e.g. `serve`); it is sanitised to one token.
    /// Returns [`LockError::Held`] immediately if another writer has the lock;
    /// this never waits.
    pub fn acquire(state: &AppState, label: &str) -> Result<RepoLock, LockError> {
        let path = state.lock_path();
        // `truncate(false)` on purpose: a losing acquirer must not wipe the record the
        // current holder wrote. Truncation happens only after we own the lock.
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(LockError::Held {
                    holder: read_holder(&path),
                    path,
                });
            }
            Err(TryLockError::Error(e)) => return Err(LockError::Io(e)),
        }
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let record = format!(
            "pid={} program={} since={since}\n",
            std::process::id(),
            sanitise(label)
        );
        write_record(&file, &record)?;
        Ok(RepoLock { file, path })
    }

    /// The lock file's path (for messages and tests).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        // Best effort: blank the record so a later contender never reports a
        // holder that has gone. The OS releases the lock itself on close.
        let _ = self.file.set_len(0);
    }
}

/// The holder's diagnostic record, or `unknown`. Empty means the holder has
/// locked but not yet written; on Windows an exclusive lock also blocks this
/// read. Neither is an error: the lock is held either way.
fn read_holder(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn write_record(mut file: &File, record: &str) -> io::Result<()> {
    file.set_len(0)?;
    file.write_all(record.as_bytes())?;
    file.flush()
}

/// One printable token: anything outside `[A-Za-z0-9._/-]` becomes `-`.
fn sanitise(label: &str) -> String {
    let token: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if token.is_empty() {
        "unnamed".to_string()
    } else {
        token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> AppState {
        let dir = std::env::temp_dir().join(format!("bw-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AppState::at(dir).unwrap()
    }

    #[test]
    fn second_acquire_in_process_is_refused_and_names_holder() {
        let state = scratch("in-process");
        let first = RepoLock::acquire(&state, "first writer").unwrap();
        assert!(first.path().exists());
        match RepoLock::acquire(&state, "second") {
            Err(LockError::Held { holder, path }) => {
                assert!(
                    holder.contains(&format!("pid={}", std::process::id())),
                    "holder record names the pid: {holder}"
                );
                assert!(holder.contains("program=first-writer"), "{holder}");
                assert_eq!(path, state.lock_path());
            }
            other => panic!("expected Held, got {other:?}"),
        }
        drop(first);
        let again = RepoLock::acquire(&state, "second").unwrap();
        let record = std::fs::read_to_string(again.path()).unwrap();
        assert!(record.contains("program=second"), "{record}");
    }

    #[test]
    fn release_blanks_the_holder_record() {
        let state = scratch("blank");
        let lock = RepoLock::acquire(&state, "x").unwrap();
        let path = lock.path().to_path_buf();
        assert!(!std::fs::read_to_string(&path).unwrap().is_empty());
        drop(lock);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn held_error_is_worded_for_a_user() {
        let e = LockError::Held {
            holder: "pid=1 program=serve since=0".into(),
            path: PathBuf::from("/x/lock"),
        };
        assert_eq!(
            e.to_string(),
            "wiki is already in use by pid=1 program=serve since=0 (lock file /x/lock)"
        );
    }

    #[test]
    fn labels_become_one_token() {
        assert_eq!(sanitise("sidebar --write"), "sidebar---write");
        assert_eq!(sanitise(""), "unnamed");
        assert_eq!(sanitise("a/b.c_d-e"), "a/b.c_d-e");
    }
}
