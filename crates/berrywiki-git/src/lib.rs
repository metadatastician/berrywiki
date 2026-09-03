// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

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

impl CommitId {
    /// The conventional 7-character abbreviation for display. Never used to
    /// address a commit: git only ever receives the full id.
    pub fn short(&self) -> &str {
        let end = self.0.len().min(7);
        &self.0[..end]
    }
}

/// The one `log` shape every history read shares: id, subject, author date
/// and author name, separated by US (`\x1f`) so no field can swallow another.
/// It asks for nothing about paths or diffs.
const LOG_FORMAT: &str = "--format=%H%x1f%s%x1f%aI%x1f%an";

/// One line of history as read by [`GitRepo::recent`] or [`GitRepo::history`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub id: CommitId,
    /// The commit subject (first line of the message).
    pub subject: String,
    /// The author date in strict ISO 8601 (`%aI`), as git prints it.
    pub date: String,
    /// The commit author name (`%an`). In Solo this is the operator's own
    /// git identity; on a shared server it is the principal the save was
    /// attributed to.
    pub author: String,
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

/// One side of a three-way merge, named by the index stage git records it in.
///
/// While a merge is unresolved the index holds up to three blobs per
/// conflicted path instead of one. Which of them exist is the whole
/// classification: all three means both sides edited the file, a missing
/// [`Stage::Base`] means both sides created it, a missing side means that side
/// removed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Stage 1 — the common ancestor both sides started from.
    Base,
    /// Stage 2 — what the local branch has.
    Ours,
    /// Stage 3 — what the incoming commit has.
    Theirs,
}

impl Stage {
    fn number(self) -> u8 {
        match self {
            Stage::Base => 1,
            Stage::Ours => 2,
            Stage::Theirs => 3,
        }
    }
}

/// One path the index holds in more than one stage, and which stages those are.
///
/// Reported verbatim: this crate says what git recorded and leaves the meaning
/// to the layer above, which knows which paths are pages and which is the
/// generated sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmergedEntry {
    /// Repository-relative path, exactly as git spells it.
    pub path: String,
    /// Stage 1 present — the path existed in the common ancestor.
    pub base: bool,
    /// Stage 2 present — the local branch still has the path.
    pub ours: bool,
    /// Stage 3 present — the incoming commit still has the path.
    pub theirs: bool,
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
    /// A path that has to become a git argument is not valid UTF-8.
    ///
    /// Reported rather than lossily converted: a backup written to a path that
    /// is not the one the caller named is worse than a refusal.
    NonUtf8Path(PathBuf),
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
            GitError::NonUtf8Path(p) => write!(
                f,
                "{} cannot be passed to git: the path is not valid UTF-8",
                p.display()
            ),
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

    /// The commit id at the tip of the fetched upstream (`@{u}`). Call
    /// [`GitRepo::fetch`] first for it to reflect the remote.
    pub fn head_of_upstream(&self) -> Result<CommitId, GitError> {
        let out = self.checked("upstream-head", &["rev-parse", "@{u}"])?;
        Ok(CommitId(out.stdout.trim().to_string()))
    }

    /// The best common ancestor of `HEAD` and the fetched upstream — the third
    /// point a three-way reconciliation needs.
    pub fn merge_base_with_upstream(&self) -> Result<CommitId, GitError> {
        let out = self.checked("merge-base", &["merge-base", "HEAD", "@{u}"])?;
        Ok(CommitId(out.stdout.trim().to_string()))
    }

    /// The most recent `limit` commits reachable from `HEAD`, newest first.
    /// An unborn branch yields an empty list rather than an error, so a
    /// freshly initialised clone can still be described.
    pub fn recent(&self, limit: usize) -> Result<Vec<LogEntry>, GitError> {
        let n = limit.to_string();
        self.log_entries(&["log", LOG_FORMAT, "-n", n.as_str()])
    }

    /// The most recent `limit` commits that touched one repository-relative
    /// `path`, newest first. The path is passed after `--`, so a filename can
    /// never be read as an option however it begins.
    ///
    /// A path git has never seen yields an empty list rather than an error: a
    /// page written while commit-on-save is off has no commits behind it yet,
    /// and that is a page with no history, not a failure. A page that was
    /// moved has a history that ends at the move, because this read asks for
    /// no rename detection; the older commits are still in the log under the
    /// previous filename.
    pub fn history(&self, path: &str, limit: usize) -> Result<Vec<LogEntry>, GitError> {
        let n = limit.to_string();
        self.log_entries(&["log", LOG_FORMAT, "-n", n.as_str(), "--", path])
    }

    /// Run one `log` shaped by [`LOG_FORMAT`] and parse its rows. Shared so
    /// that the format string and the field order can never drift apart.
    fn log_entries(&self, args: &[&str]) -> Result<Vec<LogEntry>, GitError> {
        let out = self.exec("log", args)?;
        if !out.success {
            // The only expected failure: `HEAD` names a branch with no commits.
            if out.stderr.contains("does not have any commits") {
                return Ok(Vec::new());
            }
            return Err(GitError::Git {
                op: "log",
                stderr: out.stderr,
            });
        }
        Ok(out
            .stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let mut f = line.splitn(4, '\u{1f}');
                LogEntry {
                    id: CommitId(f.next().unwrap_or("").to_string()),
                    subject: f.next().unwrap_or("").to_string(),
                    date: f.next().unwrap_or("").to_string(),
                    author: f.next().unwrap_or("").to_string(),
                }
            })
            .collect())
    }

    /// The name of the branch `HEAD` points at, or `None` when `HEAD` is not on
    /// a branch (a detached `HEAD`). An unborn branch (no commits yet) still
    /// reports its name, so it is correctly not treated as detached.
    pub fn current_branch(&self) -> Result<Option<String>, GitError> {
        let out = self.exec(
            "branch-name",
            &["symbolic-ref", "--short", "--quiet", "HEAD"],
        )?;
        if out.success {
            Ok(Some(out.stdout.trim().to_string()))
        } else {
            Ok(None)
        }
    }

    /// Is a merge recorded as started but not concluded (`MERGE_HEAD` present)?
    ///
    /// BerryWiki never begins a merge (ADR-0010), so this is only ever true of
    /// one a person started in the wiki folder themselves. The engine reads it
    /// so the layers above can refuse to bury a half-finished merge under an
    /// ordinary commit.
    pub fn merge_in_progress(&self) -> Result<bool, GitError> {
        let out = self.exec("merge-head", &["rev-parse", "-q", "--verify", "MERGE_HEAD"])?;
        Ok(out.success)
    }

    /// Every path the index currently holds in more than one stage.
    ///
    /// Read NUL-delimited (`-z`), so a file name containing a quote, a newline
    /// or a non-ASCII byte survives verbatim rather than arriving octal-escaped
    /// as git's default quoting would render it. Each record is
    /// `<mode> <sha> <stage>\t<path>`, one per stage, so a path appears up to
    /// three times and is folded back into a single entry here. First-seen
    /// order is preserved, which is git's own sort order.
    pub fn unmerged(&self) -> Result<Vec<UnmergedEntry>, GitError> {
        let out = self.checked("ls-files", &["ls-files", "-u", "-z"])?;
        let mut entries: Vec<UnmergedEntry> = Vec::new();
        for record in out.stdout.split('\0').filter(|s| !s.is_empty()) {
            let Some((meta, path)) = record.split_once('\t') else {
                continue;
            };
            let stage = meta.split_whitespace().next_back().unwrap_or("");
            if !entries.iter().any(|e| e.path == path) {
                entries.push(UnmergedEntry {
                    path: path.to_string(),
                    base: false,
                    ours: false,
                    theirs: false,
                });
            }
            let slot = entries
                .iter_mut()
                .find(|e| e.path == path)
                .expect("entry inserted immediately above");
            match stage {
                "1" => slot.base = true,
                "2" => slot.ours = true,
                "3" => slot.theirs = true,
                _ => {}
            }
        }
        Ok(entries)
    }

    /// The content one side of an unresolved merge has for a path, or `None`
    /// when the index holds no blob at that stage — which is itself the signal
    /// that the side in question removed the file, or never had it.
    ///
    /// The object argument is always `:<stage>:<path>`, so it opens with a
    /// colon and no path, however it is spelled, can be read as an option.
    /// Bytes are decoded lossily; callers ask only about text files.
    pub fn show_stage(&self, stage: Stage, path: &str) -> Result<Option<String>, GitError> {
        let object = format!(":{}:{}", stage.number(), path);
        let out = self.exec("show", &["show", object.as_str()])?;
        Ok(if out.success { Some(out.stdout) } else { None })
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

    /// Stage everything and conclude a merge someone else already started,
    /// returning the resulting commit id.
    ///
    /// Unlike [`GitRepo::commit_all`] this always commits: concluding a merge
    /// is meaningful even when the merged tree happens to match `HEAD`, and
    /// stopping short would strand the clone with `MERGE_HEAD` still set. It
    /// records the second parent git already noted, so no history is replaced
    /// and nothing is discarded. Verifying that no path is still unmerged is
    /// the caller's job — staging conflict markers would bury them.
    pub fn commit_merge(&self, message: &str) -> Result<CommitId, GitError> {
        self.checked("stage", &["add", "-A"])?;
        self.checked("commit", &["commit", "--quiet", "-m", message])?;
        self.head()
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

    // ----- archival -----

    /// Write every ref, and `HEAD`, into a single-file git bundle.
    ///
    /// A bundle is an ordinary git transport rather than a BerryWiki format:
    /// `git clone` reads one directly, so a backup stays recoverable with git
    /// alone and nothing here needs to exist to get the content back.
    ///
    /// The honest limit, and the reason the CLI refuses a dirty tree: a bundle
    /// carries **committed history only**. Work that is still uncommitted —
    /// which is exactly what `serve --no-commit` leaves behind — is not in it.
    ///
    /// `out_file` is resolved against the *process* working directory, not the
    /// repository's, so a relative path means what the person who typed it
    /// meant rather than landing inside the wiki.
    pub fn bundle_all(&self, out_file: &Path) -> Result<(), GitError> {
        let out_file = std::path::absolute(out_file).map_err(GitError::Io)?;
        let path = utf8_arg(&out_file)?;
        self.checked("bundle", &["bundle", "create", path, "--all"])?;
        Ok(())
    }

    /// The URL configured for `origin`, or `None` when there is no such remote.
    ///
    /// Absence is not an error: a wiki that was never cloned from anywhere is
    /// a legitimate thing to back up.
    pub fn origin_url(&self) -> Result<Option<String>, GitError> {
        let out = self.exec("remote", &["remote", "get-url", "origin"])?;
        if !out.success {
            return Ok(None);
        }
        let url = out.stdout.trim();
        Ok((!url.is_empty()).then(|| url.to_string()))
    }

    /// Point `origin` at `url`, adding the remote when it is absent.
    ///
    /// This edits this clone's config. No ref is moved, no object is written,
    /// and nothing on any remote is contacted or changed.
    pub fn set_origin_url(&self, url: &str) -> Result<(), GitError> {
        if self.origin_url()?.is_some() {
            self.checked("remote", &["remote", "set-url", "origin", url])?;
        } else {
            self.checked("remote", &["remote", "add", "origin", url])?;
        }
        Ok(())
    }

    /// Drop the `origin` remote. Config only, with the same guarantee as
    /// [`set_origin_url`](Self::set_origin_url): local history is untouched and
    /// no remote is contacted.
    ///
    /// This exists because a clone taken from a bundle has an `origin` naming
    /// the bundle *file*, which is not a remote anyone can fetch from once the
    /// file is gone. Leaving it in place would be a lie about where the wiki
    /// came from.
    pub fn forget_origin(&self) -> Result<(), GitError> {
        if self.origin_url()?.is_some() {
            self.checked("remote", &["remote", "remove", "origin"])?;
        }
        Ok(())
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
        let mut cmd = hermetic_command();
        cmd.current_dir(&self.workdir)
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

/// A `git` invocation with the ambient environment already neutralised.
///
/// Every git command in this crate starts here, including the ones that run
/// outside any working tree, so hermeticity is one definition rather than a
/// habit repeated per call site and forgotten once.
fn hermetic_command() -> Command {
    let mut cmd = Command::new("git");
    cmd
        // Stable, parseable output regardless of the ambient locale.
        .env("LC_ALL", "C")
        // Neutralise ambient config: no user/system aliases, hooks or
        // signing can change what these commands do.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        // Never block waiting for interactive credentials.
        .env("GIT_TERMINAL_PROMPT", "0")
        // Read-only commands shouldn't take the index lock.
        .env("GIT_OPTIONAL_LOCKS", "0");
    cmd
}

/// A path as a git argument, or a refusal if it is not UTF-8.
fn utf8_arg(path: &Path) -> Result<&str, GitError> {
    path.to_str()
        .ok_or_else(|| GitError::NonUtf8Path(path.to_path_buf()))
}

/// Build a working tree from a bundle written by [`GitRepo::bundle_all`].
///
/// `dest` must not already exist as a non-empty directory; git refuses
/// otherwise, and that refusal is reported rather than worked around, because
/// the alternative is writing over somebody's clone.
///
/// The resulting clone's `origin` names the *bundle file*, which is an artefact
/// of how bundles are transported rather than a remote anything can fetch from
/// later. Callers are expected to repoint it with
/// [`GitRepo::set_origin_url`] or drop it with [`GitRepo::forget_origin`];
/// the CLI's recovery command does one or the other every time.
///
/// (That command cannot be named here. The audit test scans this file for the
/// tokens of working-tree-discarding git operations and does not exempt
/// comments, so the engine may not even mention them in prose. The bluntness
/// is the point: there is no "it was only a comment" hole to slip through.)
pub fn clone_from_bundle(bundle: &Path, dest: &Path) -> Result<GitRepo, GitError> {
    let bundle = std::path::absolute(bundle).map_err(GitError::Io)?;
    let dest = std::path::absolute(dest).map_err(GitError::Io)?;
    let mut cmd = hermetic_command();
    cmd.args(["clone", utf8_arg(&bundle)?, utf8_arg(&dest)?]);
    let out = cmd.output().map_err(GitError::Io)?;
    if !out.status.success() {
        return Err(GitError::Git {
            op: "clone",
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    GitRepo::open(dest)
}
