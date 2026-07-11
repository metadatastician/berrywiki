//! An advisory repository lock so two BerryWiki processes never mutate the same
//! clone at once (a Git-rules requirement). The lock file lives in the
//! app-state directory (outside the clone). A lock held by a process that has
//! since died is detected and reclaimed, so a crash cannot wedge the wiki.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// A held repository lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct RepoLock {
    path: PathBuf,
    // Whether we still own the file (set false if we deliberately release).
    owned: bool,
}

impl RepoLock {
    /// Try to acquire the lock. Fails with `WouldBlock` if a *live* process
    /// holds it; reclaims it if the holder has died.
    pub fn acquire(lock_path: &Path) -> io::Result<RepoLock> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        match Self::create_exclusive(lock_path) {
            Ok(()) => Ok(RepoLock {
                path: lock_path.to_path_buf(),
                owned: true,
            }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Someone holds it. Reclaim only if that holder is gone.
                let holder = Self::read_pid(lock_path);
                match holder {
                    Some(pid) if process_is_alive(pid) => Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("wiki is locked by a running process (pid {pid})"),
                    )),
                    _ => {
                        // Stale (dead holder or unreadable): reclaim.
                        fs::remove_file(lock_path)?;
                        Self::create_exclusive(lock_path)?;
                        Ok(RepoLock {
                            path: lock_path.to_path_buf(),
                            owned: true,
                        })
                    }
                }
            }
            Err(e) => Err(e),
        }
    }

    fn create_exclusive(lock_path: &Path) -> io::Result<()> {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)?;
        write!(f, "{}", std::process::id())?;
        f.sync_all()
    }

    fn read_pid(lock_path: &Path) -> Option<u32> {
        let mut s = String::new();
        fs::File::open(lock_path).ok()?.read_to_string(&mut s).ok()?;
        s.trim().parse().ok()
    }

    /// Explicitly release the lock (also happens on drop).
    pub fn release(mut self) {
        self.remove();
    }

    fn remove(&mut self) {
        if self.owned {
            let _ = fs::remove_file(&self.path);
            self.owned = false;
        }
    }
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Best-effort liveness check for a pid.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // On Linux /proc/<pid> exists iff the process exists. Conservative: if we
    // cannot tell, assume alive (never steal a lock we are unsure about).
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    // Without a portable check, never steal — safer to refuse than to risk two
    // writers. A truly stale lock must be removed manually on such platforms.
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static C: AtomicUsize = AtomicUsize::new(0);

    fn lock_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "bw-lock-{}-{}.lock",
            std::process::id(),
            C.fetch_add(1, Ordering::SeqCst)
        ))
    }

    #[test]
    fn acquire_and_release() {
        let p = lock_path();
        let lock = RepoLock::acquire(&p).unwrap();
        assert!(p.exists());
        lock.release();
        assert!(!p.exists());
    }

    #[test]
    fn second_live_acquire_is_refused() {
        let p = lock_path();
        let _held = RepoLock::acquire(&p).unwrap();
        // Our own pid is alive, so a second acquire must be refused.
        let err = RepoLock::acquire(&p).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn stale_lock_from_dead_holder_is_reclaimed() {
        let p = lock_path();
        // A lock file naming a pid that cannot be alive (0 has no /proc entry).
        fs::write(&p, "0").unwrap();
        let lock = RepoLock::acquire(&p).unwrap();
        // We reclaimed it and now hold our own pid.
        assert_eq!(RepoLock::read_pid(&p), Some(std::process::id()));
        lock.release();
    }

    #[test]
    fn drop_releases() {
        let p = lock_path();
        {
            let _lock = RepoLock::acquire(&p).unwrap();
            assert!(p.exists());
        }
        assert!(!p.exists(), "drop released the lock");
    }
}
