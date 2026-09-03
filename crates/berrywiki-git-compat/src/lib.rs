// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Test-support sandbox for reproducing git synchronisation situations.
//!
//! A [`GitSandbox`] is a bare "remote" repository plus two working clones
//! ("ours" and "theirs") seeded from the fixture wiki. Tests use it to
//! reproduce exactly the situations BerryWiki's sync layer must survive:
//! remote changes, non-fast-forward pushes, same-page merge conflicts and
//! modify/delete conflicts — and to prove that **local work is never lost**.
//!
//! This crate intentionally shells out to the `git` binary: its job is to
//! gather *evidence* of real git behaviour for the compatibility report, not
//! to be the production sync engine (that choice is a pending ADR).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Every sandbox directory name starts with this. `Drop` checks for it before
/// removing anything, so the prefix is a safety property and not just a name.
const PREFIX: &str = "berrywiki-git-compat-";

/// Set this to `1` to keep sandboxes on disk after the tests that made them,
/// for when a failure needs the repository inspected. The path is printed.
pub const KEEP_ENV: &str = "BERRYWIKI_KEEP_SANDBOX";

/// Output of one git invocation.
#[derive(Debug)]
pub struct GitResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl GitResult {
    /// Panic with full context when a command that must succeed did not.
    pub fn expect_success(self, what: &str) -> Self {
        assert!(
            self.success,
            "{what} failed:\nstdout: {}\nstderr: {}",
            self.stdout, self.stderr
        );
        self
    }
}

/// A bare remote plus two clones, all under one scratch directory.
///
/// The directory is removed when the value is dropped, so a sandbox lives
/// exactly as long as the test that owns it. Set `BERRYWIKI_KEEP_SANDBOX=1`
/// to keep it for inspection; the path is printed on the way out.
pub struct GitSandbox {
    pub root: PathBuf,
    pub remote: PathBuf,
    pub ours: PathBuf,
    pub theirs: PathBuf,
}

impl GitSandbox {
    /// Create the sandbox: bare remote, clone "ours", seed it with every file
    /// from `seed_dir` (top level only), commit, push, then clone "theirs".
    pub fn create(seed_dir: &Path) -> GitSandbox {
        let root = std::env::temp_dir().join(format!(
            "{PREFIX}{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let remote = root.join("remote.git");
        let ours = root.join("ours");
        let theirs = root.join("theirs");
        fs::create_dir_all(&remote).expect("create sandbox dirs");

        git_in(&root, &["init", "--bare", "-b", "main", "remote.git"])
            .expect_success("init bare remote");
        git_in(&root, &["clone", remote.to_str().unwrap(), "ours"]).expect_success("clone ours");

        // Seed from the fixture (files only; the fixture itself is read-only).
        for entry in fs::read_dir(seed_dir).expect("read seed dir") {
            let path = entry.expect("seed entry").path();
            if path.is_file() {
                fs::copy(&path, ours.join(path.file_name().unwrap())).expect("seed copy");
            }
        }

        let sandbox = GitSandbox {
            root,
            remote,
            ours,
            theirs,
        };
        sandbox
            .git(&sandbox.ours.clone(), &["add", "-A"])
            .expect_success("stage seed");
        sandbox
            .git(
                &sandbox.ours.clone(),
                &["commit", "-m", "Seed fixture wiki"],
            )
            .expect_success("commit seed");
        sandbox
            .git(&sandbox.ours.clone(), &["push", "origin", "main"])
            .expect_success("push seed");
        git_in(
            &sandbox.root,
            &["clone", sandbox.remote.to_str().unwrap(), "theirs"],
        )
        .expect_success("clone theirs");
        sandbox
    }

    /// Run git in one of the sandbox working copies.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> GitResult {
        git_in(cwd, args)
    }

    /// Overwrite a file and commit it in the given clone.
    pub fn commit_change(&self, clone: &Path, file: &str, content: &str, message: &str) {
        fs::write(clone.join(file), content).expect("write change");
        self.git(clone, &["add", file]).expect_success("stage");
        self.git(clone, &["commit", "-m", message])
            .expect_success("commit");
    }

    /// Current HEAD commit id of a clone.
    pub fn head(&self, clone: &Path) -> String {
        self.git(clone, &["rev-parse", "HEAD"])
            .expect_success("rev-parse")
            .stdout
            .trim()
            .to_string()
    }

    /// Number of remote commits not yet in the local branch (after `fetch`).
    pub fn behind_by(&self, clone: &Path) -> usize {
        self.git(clone, &["fetch", "origin"])
            .expect_success("fetch");
        let out = self
            .git(clone, &["rev-list", "--count", "HEAD..origin/main"])
            .expect_success("rev-list");
        out.stdout.trim().parse().unwrap_or(0)
    }
}

/// Run git with a hermetic identity and no system/user config interference.
fn git_in(cwd: &Path, args: &[&str]) -> GitResult {
    let output = Command::new("git")
        .current_dir(cwd)
        // Hermetic: no ~/.gitconfig hooks/signing/aliases can distort evidence.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "BerryWiki Test")
        .env("GIT_AUTHOR_EMAIL", "test@berrywiki.invalid")
        .env("GIT_COMMITTER_NAME", "BerryWiki Test")
        .env("GIT_COMMITTER_EMAIL", "test@berrywiki.invalid")
        .args(args)
        .output()
        .expect("failed to spawn git — is git installed?");
    GitResult {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Whether the sandbox directory should survive the test that made it.
///
/// Split out from [`Drop`] so it can be tested without mutating the process
/// environment, which is racy: a test binary runs its tests on many threads
/// and `set_var` is visible to all of them at once.
fn keep_requested(value: Option<&str>) -> bool {
    matches!(value, Some("1"))
}

/// True only for a path this crate minted: directly inside the temp directory
/// and carrying [`PREFIX`].
///
/// `GitSandbox::root` is a public field, so nothing in the type system stops a
/// caller pointing it somewhere real before the value is dropped. This check
/// is what makes the recursive delete below safe to write at all: anything
/// that does not look like ours is left exactly where it is.
fn is_our_sandbox(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    path.parent() == Some(temp.as_path())
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(PREFIX))
}

impl Drop for GitSandbox {
    /// Remove the whole sandbox.
    ///
    /// Every sandbox is a bare repository plus two clones, so it is three git
    /// object stores' worth of small files. A test binary makes one per test;
    /// left behind they accumulate until the temp filesystem runs out of
    /// *inodes*, which `df -h` does not show and which then surfaces as
    /// unrelated tests failing to write.
    fn drop(&mut self) {
        if keep_requested(std::env::var(KEEP_ENV).ok().as_deref()) {
            eprintln!("{KEEP_ENV}=1: kept {}", self.root.display());
            return;
        }
        if !is_our_sandbox(&self.root) {
            return;
        }
        // Errors are swallowed deliberately. A panic here would fire during
        // unwinding from the test failure that is being reported, which aborts
        // the process and loses the failure. A sandbox that cannot be removed
        // is the lesser problem, and the next run's name is different anyway.
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/test-wiki")
    }

    #[test]
    fn dropping_a_sandbox_removes_its_directory() {
        // This test is about the very flag a developer sets when they are
        // debugging some *other* failure, so it asserts whichever branch it
        // was actually run in rather than failing in their face.
        let keep = keep_requested(std::env::var(KEEP_ENV).ok().as_deref());

        let root = {
            let sb = GitSandbox::create(&fixture_dir());
            assert!(sb.root.is_dir(), "the sandbox was not created");
            sb.root.clone()
        };

        if keep {
            assert!(
                root.exists(),
                "{KEEP_ENV} was set and the sandbox was removed anyway: {}",
                root.display()
            );
            // Kept on request, but this one was made by a test, so it is this
            // test's to clear up.
            let _ = fs::remove_dir_all(&root);
        } else {
            assert!(
                !root.exists(),
                "the sandbox outlived the value that owned it: {}",
                root.display()
            );
        }
    }

    #[test]
    fn only_paths_this_crate_minted_are_removable() {
        let temp = std::env::temp_dir();
        assert!(is_our_sandbox(&temp.join(format!("{PREFIX}1-0"))));

        // The cases that matter are the ones a recursive delete must refuse.
        assert!(!is_our_sandbox(&temp), "the temp directory itself");
        assert!(
            !is_our_sandbox(&temp.join("something-else")),
            "wrong prefix"
        );
        assert!(
            !is_our_sandbox(&temp.join(format!("{PREFIX}1-0")).join("ours")),
            "a clone inside a sandbox is not itself a sandbox"
        );
        assert!(
            !is_our_sandbox(Path::new("/home/someone/berrywiki-git-compat-1-0")),
            "the prefix alone is not enough; it must be in the temp directory"
        );
    }

    #[test]
    fn only_the_exact_value_one_keeps_a_sandbox() {
        assert!(keep_requested(Some("1")));
        assert!(!keep_requested(None));
        assert!(!keep_requested(Some("")));
        assert!(!keep_requested(Some("0")));
        // "true" and "yes" are deliberately not accepted: one spelling means
        // the flag either worked or plainly did not, with no near-miss.
        assert!(!keep_requested(Some("true")));
    }
}
