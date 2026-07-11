//! `berrywiki-git` — a deliberately small, safe wrapper over the `git` CLI.
//!
//! This is the production sync engine's contact point with git. Its entire job
//! is to make the *safe* synchronisation operations easy and the *unsafe* ones
//! impossible:
//!
//! * commit the user's edits ([`GitRepo::commit_all`]),
//! * learn how local and remote relate ([`GitRepo::fetch`],
//!   [`GitRepo::divergence`]),
//! * advance to the remote only when that cannot lose anything
//!   ([`GitRepo::fast_forward_to_upstream`]),
//! * publish local commits only when they extend the remote
//!   ([`GitRepo::push`]).
//!
//! # Safety by construction
//!
//! The set of git invocations is *closed*: every one is a fixed argument list
//! built from string literals in this file. There is no method, and no code
//! path, that appends a history-overwriting or working-tree-discarding flag or
//! subcommand — the source contains none of the tokens that would do so, which
//! `tests/audit.rs` verifies by scanning this file. A rejected push therefore
//! preserves both local and remote history, and integration is fast-forward
//! only, never a working-tree rewind. Data safety is thus structural, not a
//! convention we merely try to follow (see ADR-0009).
//!
//! # Hermetic execution
//!
//! Every invocation runs with `LC_ALL=C` (stable, parseable output), the user
//! and system git config neutralised (no aliases, hooks or signing can distort
//! behaviour), terminal prompting disabled (`GIT_TERMINAL_PROMPT=0`, so a
//! missing credential fails fast instead of hanging), optional locks off, and
//! an explicit author/committer identity so a commit never depends on ambient
//! `user.name`/`user.email`. Credentials for a real remote are supplied by the
//! caller through [`GitRepo::with_env`] (e.g. a `GIT_ASKPASS` helper), which
//! survives the config neutralisation because it is an environment variable,
//! not config.
//!
//! This wrapper is Unix/WSL-oriented (it points the config knobs at
//! `/dev/null`), matching BerryWiki's rule that git only ever runs against the
//! wiki clone from inside WSL.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The author/committer recorded on commits BerryWiki makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Default for Identity {
    fn default() -> Self {
        Identity {
            name: "BerryWiki".to_string(),
            email: "berrywiki@localhost".to_string(),
        }
    }
}

/// A commit object name (the full 40-hex id as git reports it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitId(pub String);

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How the local branch and its fetched upstream relate.
///
/// `ahead`/`behind` are counted against the *already-fetched* upstream, so the
/// caller is expected to [`GitRepo::fetch`] first for these to be current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// Local commits the upstream does not have.
    pub ahead: usize,
    /// Upstream commits the local branch does not have.
    pub behind: usize,
    /// Whether an upstream is configured at all.
    pub has_upstream: bool,
}

impl Divergence {
    /// Local and upstream point at the same commit.
    pub fn is_up_to_date(&self) -> bool {
        self.has_upstream && self.ahead == 0 && self.behind == 0
    }

    /// The upstream is strictly ahead — integration is a clean fast-forward.
    pub fn can_fast_forward(&self) -> bool {
        self.has_upstream && self.ahead == 0 && self.behind > 0
    }

    /// Both sides moved — histories have diverged and a merge is required.
    pub fn needs_merge(&self) -> bool {
        self.has_upstream && self.ahead > 0 && self.behind > 0
    }

    /// Local is strictly ahead — there is something safe to publish.
    pub fn can_publish(&self) -> bool {
        self.has_upstream && self.ahead > 0 && self.behind == 0
    }
}

/// Outcome of trying to advance the local branch to its fetched upstream,
/// fast-forward only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrateOutcome {
    /// Nothing to do — local already contained the upstream.
    AlreadyUpToDate,
    /// Local advanced to the upstream commit with no new merge commit.
    FastForwarded,
    /// The histories diverged; local was left exactly as it was and the caller
    /// must merge deliberately. Nothing was integrated and nothing was lost.
    NeedsManualMerge,
    /// No upstream is configured.
    NoUpstream,
}

/// Outcome of publishing local commits. No variant involves overwriting the
/// remote: a rejection is reported, never worked around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// Local commits were published; the remote fast-forwarded.
    Pushed,
    /// The remote already had everything local did.
    UpToDate,
    /// The remote moved on; publishing would need to overwrite it, so git
    /// declined. Local history is intact and the remote is untouched — the
    /// caller should fetch, integrate, and try again.
    RejectedNonFastForward,
    /// No upstream is configured to publish to.
    NoUpstream,
}

/// Working-tree status (the pending changes, if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// One entry per pending change, as porcelain reports it (`XY path`).
    pub entries: Vec<String>,
}

impl Status {
    /// No pending changes — the working tree matches `HEAD` and the index.
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Something went wrong talking to git.
#[derive(Debug)]
pub enum GitError {
    /// The path is not inside a git working tree.
    NotARepo(PathBuf),
    /// A git command failed unexpectedly.
    Git { op: &'static str, stderr: String },
    /// The git binary could not be spawned at all.
    Io(std::io::Error),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::NotARepo(p) => {
                write!(f, "{} is not inside a git working tree", p.display())
            }
            GitError::Git { op, stderr } => {
                write!(f, "git {op} failed: {}", stderr.trim())
            }
            GitError::Io(e) => write!(f, "could not run git: {e}"),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GitError::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// The captured result of one git invocation.
struct Run {
    success: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A safe handle onto one git working tree.
pub struct GitRepo {
    workdir: PathBuf,
    identity: Identity,
    extra_env: Vec<(OsString, OsString)>,
}

impl GitRepo {
    /// Open an existing working tree, verifying it really is one.
    pub fn open(workdir: impl AsRef<Path>) -> Result<GitRepo, GitError> {
        let repo = GitRepo {
            workdir: workdir.as_ref().to_path_buf(),
            identity: Identity::default(),
            extra_env: Vec::new(),
        };
        let out = repo.exec("open", &["rev-parse", "--is-inside-work-tree"])?;
        if !out.success || out.stdout.trim() != "true" {
            return Err(GitError::NotARepo(repo.workdir));
        }
        Ok(repo)
    }

    /// Set the author/committer identity used for commits.
    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }

    /// Attach an environment variable to every git invocation. This is how a
    /// caller injects credentials (e.g. `GIT_ASKPASS` and a token) for a real
    /// remote without weakening the hermetic config isolation.
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    /// The working tree this handle operates on.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    // ----- read-only inspection -----

    /// The current `HEAD` commit id.
    pub fn head(&self) -> Result<CommitId, GitError> {
        let out = self.checked("head", &["rev-parse", "HEAD"])?;
        Ok(CommitId(out.stdout.trim().to_string()))
    }

    /// Pending working-tree changes (NUL-delimited porcelain, so unusual file
    /// names are handled).
    ///
    /// A rename or copy is a single logical change that git encodes in the `-z`
    /// stream as *two* NUL-terminated fields — the new path (after the `XY`
    /// code) and then the original path. We fold those back into one entry
    /// rendered as `XY <old> -> <new>`, so the count is right and every entry
    /// keeps its status prefix.
    pub fn status(&self) -> Result<Status, GitError> {
        let out = self.checked("status", &["status", "--porcelain", "-z"])?;
        let mut fields = out.stdout.split('\0').filter(|s| !s.is_empty());
        let mut entries = Vec::new();
        while let Some(record) = fields.next() {
            // `XY path`: a rename (R) or copy (C) in either status column is
            // followed by one more field, the original path.
            let is_rename_or_copy = record
                .as_bytes()
                .get(0..2)
                .map(|xy| xy.contains(&b'R') || xy.contains(&b'C'))
                .unwrap_or(false);
            if is_rename_or_copy {
                if let Some(origin) = fields.next() {
                    // Split off the fixed `XY ` prefix; the remainder is the new
                    // path. Byte 3 is always a char boundary (the prefix is
                    // ASCII status codes plus a space).
                    let cut = record.len().min(3);
                    let (prefix, new_path) = record.split_at(cut);
                    entries.push(format!("{prefix}{origin} -> {new_path}"));
                    continue;
                }
            }
            entries.push(record.to_string());
        }
        Ok(Status { entries })
    }

    /// Whether the working tree has no pending changes.
    pub fn is_clean(&self) -> Result<bool, GitError> {
        Ok(self.status()?.is_clean())
    }

    /// How the local branch relates to its fetched upstream. Call
    /// [`GitRepo::fetch`] first for the counts to reflect the remote.
    pub fn divergence(&self) -> Result<Divergence, GitError> {
        if !self.has_upstream()? {
            return Ok(Divergence {
                ahead: 0,
                behind: 0,
                has_upstream: false,
            });
        }
        let ahead = self.count(&["rev-list", "--count", "@{u}..HEAD"])?;
        let behind = self.count(&["rev-list", "--count", "HEAD..@{u}"])?;
        Ok(Divergence {
            ahead,
            behind,
            has_upstream: true,
        })
    }

    // ----- committing -----

    /// Stage every change and commit it, returning the new commit id — or
    /// `None` when there was nothing to commit (a no-op, never an empty
    /// commit). Assumes no merge is in progress; conflict resolution is a
    /// separate concern.
    pub fn commit_all(&self, message: &str) -> Result<Option<CommitId>, GitError> {
        self.checked("stage", &["add", "-A"])?;
        // `diff --cached --quiet` exits 0 when the index matches HEAD (nothing
        // staged) and 1 when it differs. Anything else is a real failure.
        let staged = self.exec("diff", &["diff", "--cached", "--quiet"])?;
        if staged.code == Some(0) {
            return Ok(None);
        }
        if staged.code != Some(1) {
            return Err(GitError::Git {
                op: "diff",
                stderr: staged.stderr,
            });
        }
        self.checked("commit", &["commit", "--quiet", "-m", message])?;
        Ok(Some(self.head()?))
    }

    // ----- remote synchronisation -----

    /// Update remote-tracking refs from the configured remote. Does not touch
    /// the working tree.
    pub fn fetch(&self) -> Result<(), GitError> {
        self.checked("fetch", &["fetch"])?;
        Ok(())
    }

    /// Advance the local branch to its fetched upstream, fast-forward only.
    ///
    /// This can only move `HEAD` forward to a descendant the upstream already
    /// points at; if the histories have diverged it makes no change and reports
    /// [`IntegrateOutcome::NeedsManualMerge`]. It never creates a merge commit
    /// and never rewinds the working tree, so no local work can be lost.
    pub fn fast_forward_to_upstream(&self) -> Result<IntegrateOutcome, GitError> {
        if !self.has_upstream()? {
            return Ok(IntegrateOutcome::NoUpstream);
        }
        let before = self.head()?;
        let merged = self.exec("integrate", &["merge", "--ff-only", "@{u}"])?;
        if !merged.success {
            // The only expected failure is "not possible to fast-forward";
            // either way local is unchanged, which the caller can rely on.
            return Ok(IntegrateOutcome::NeedsManualMerge);
        }
        if self.head()? == before {
            Ok(IntegrateOutcome::AlreadyUpToDate)
        } else {
            Ok(IntegrateOutcome::FastForwarded)
        }
    }

    /// Publish local commits to the upstream. The remote only advances if local
    /// strictly extends it; if the remote moved on, git rejects the push and we
    /// report [`PushOutcome::RejectedNonFastForward`] — the remote is left
    /// exactly as it was and local history is intact. The caller then fetches,
    /// integrates, and retries. There is no path that overwrites the remote.
    pub fn push(&self) -> Result<PushOutcome, GitError> {
        if !self.has_upstream()? {
            return Ok(PushOutcome::NoUpstream);
        }
        let pushed = self.exec("push", &["push"])?;
        if pushed.success {
            let combined = format!("{}{}", pushed.stdout, pushed.stderr);
            if combined.contains("Everything up-to-date") {
                return Ok(PushOutcome::UpToDate);
            }
            return Ok(PushOutcome::Pushed);
        }
        let why = pushed.stderr.to_lowercase();
        // A client-side non-fast-forward — the remote moved on — always carries
        // one of these parentheticals. We deliberately do NOT key on the bare
        // word "rejected": a server-side decline is reported as "[remote
        // rejected]" (a pre-receive/update hook, a protected branch, secret
        // push-protection), which is a real, non-recoverable error, not the
        // transient "fetch, integrate, retry" case. Masking it here would throw
        // away the diagnostic and send the caller into a fruitless retry loop,
        // so anything that is not clearly a non-fast-forward falls through to a
        // surfaced error below.
        if why.contains("non-fast-forward") || why.contains("fetch first") {
            Ok(PushOutcome::RejectedNonFastForward)
        } else if why.contains("no upstream") || why.contains("no configured push destination") {
            Ok(PushOutcome::NoUpstream)
        } else {
            Err(GitError::Git {
                op: "push",
                stderr: pushed.stderr,
            })
        }
    }

    // ----- internals -----

    /// Is an upstream tracking branch configured for the current branch?
    fn has_upstream(&self) -> Result<bool, GitError> {
        let out = self.exec(
            "upstream",
            &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )?;
        Ok(out.success)
    }

    /// Run a command whose stdout is a single integer count.
    fn count(&self, args: &[&str]) -> Result<usize, GitError> {
        let out = self.checked("rev-list", args)?;
        Ok(out.stdout.trim().parse().unwrap_or(0))
    }

    /// Run git and require success, mapping failure to [`GitError::Git`].
    fn checked(&self, op: &'static str, args: &[&str]) -> Result<Run, GitError> {
        let out = self.exec(op, args)?;
        if !out.success {
            return Err(GitError::Git {
                op,
                stderr: out.stderr,
            });
        }
        Ok(out)
    }

    /// Run git hermetically and capture the result. Non-zero exit is returned,
    /// not treated as an error, so callers can inspect expected failures.
    fn exec(&self, _op: &'static str, args: &[&str]) -> Result<Run, GitError> {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.workdir)
            // Stable, parseable output regardless of the ambient locale.
            .env("LC_ALL", "C")
            // Neutralise ambient config: no user/system aliases, hooks or
            // signing can change what these commands do.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            // Never block waiting for interactive credentials.
            .env("GIT_TERMINAL_PROMPT", "0")
            // Read-only commands shouldn't take the index lock.
            .env("GIT_OPTIONAL_LOCKS", "0")
            // A commit never depends on ambient user.name/user.email.
            .env("GIT_AUTHOR_NAME", &self.identity.name)
            .env("GIT_AUTHOR_EMAIL", &self.identity.email)
            .env("GIT_COMMITTER_NAME", &self.identity.name)
            .env("GIT_COMMITTER_EMAIL", &self.identity.email);
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        cmd.args(args);
        let out = cmd.output().map_err(GitError::Io)?;
        Ok(Run {
            success: out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}
