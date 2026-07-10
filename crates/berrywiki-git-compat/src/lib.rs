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
            "berrywiki-git-compat-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let remote = root.join("remote.git");
        let ours = root.join("ours");
        let theirs = root.join("theirs");
        fs::create_dir_all(&remote).expect("create sandbox dirs");

        git_in(&root, &["init", "--bare", "-b", "main", "remote.git"])
            .expect_success("init bare remote");
        git_in(&root, &["clone", remote.to_str().unwrap(), "ours"])
            .expect_success("clone ours");

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
            .git(&sandbox.ours.clone(), &["commit", "-m", "Seed fixture wiki"])
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
        self.git(clone, &["fetch", "origin"]).expect_success("fetch");
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
