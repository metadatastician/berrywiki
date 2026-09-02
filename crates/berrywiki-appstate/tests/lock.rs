// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Two-process evidence for the single-writer lock (X-lock): a second
//! *process* is refused while the first holds the lock, and succeeds once the
//! first is killed, with no reclaim step in between. The helper is this same
//! test binary re-invoked with an environment variable set.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use berrywiki_appstate::{AppState, LockError, RepoLock};

const HELPER_ENV: &str = "BERRYWIKI_LOCK_HELPER_DIR";

/// Not a test of anything on its own: when `BERRYWIKI_LOCK_HELPER_DIR` is set
/// it takes the lock in that directory, prints `held`, and blocks until killed.
#[test]
fn helper_hold_lock_until_killed() {
    let Some(dir) = std::env::var_os(HELPER_ENV) else {
        return; // ordinary test run: nothing to do
    };
    let state = AppState::at(dir).expect("helper app state");
    let _lock = RepoLock::acquire(&state, "helper").expect("helper acquires");
    println!("held");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

#[test]
fn second_process_is_refused_until_holder_dies() {
    let dir = std::env::temp_dir().join(format!("bw-lock-xproc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = AppState::at(&dir).unwrap();

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "helper_hold_lock_until_killed", "--nocapture"])
        .env(HELPER_ENV, &dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn helper");
    let stdout = child.stdout.take().unwrap();

    // Wait for the sentinel on a thread so a wedged helper fails the test
    // instead of hanging it.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) if l.trim() == "held" => {
                    let _ = tx.send(true);
                    return;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let _ = tx.send(false);
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(true) => {}
        other => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("helper never reported `held`: {other:?}");
        }
    }

    // While the helper lives, we are refused and told who holds it.
    match RepoLock::acquire(&state, "parent") {
        Err(LockError::Held { holder, .. }) => {
            assert!(
                holder.contains(&format!("pid={}", child.id())),
                "holder names the helper pid: {holder}"
            );
            assert!(holder.contains("program=helper"), "{holder}");
        }
        other => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("expected Held while helper lives, got {other:?}");
        }
    }

    // Kill it (no clean-up path runs) and the OS releases the lock.
    child.kill().expect("kill helper");
    child.wait().expect("reap helper");
    let lock = RepoLock::acquire(&state, "parent").expect("lock free after holder death");
    let record = std::fs::read_to_string(lock.path()).unwrap();
    assert!(record.contains("program=parent"), "{record}");
}
