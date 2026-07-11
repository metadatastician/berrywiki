//! `berrywiki-sync` — the store ⇄ git wiring.
//!
//! [`SyncedStore`] owns a [`WikiStore`] and a [`GitRepo`] over the *same*
//! working tree and gives BerryWiki two guarantees:
//!
//! 1. **Commit-on-save.** Every committing mutation ([`SyncedStore::create_page`]
//!    and friends) performs the store change and then makes exactly **one**
//!    atomic git commit capturing it — the regenerated `_Sidebar.md` included,
//!    because the store regenerates it in the same call. One completed mutation
//!    is one logical commit; the caller cannot forget to commit or bundle two
//!    edits together.
//! 2. **Safe synchronisation.** [`SyncedStore::sync`] runs
//!    fetch → integrate (fast-forward only) → push. A history that has genuinely
//!    diverged (local *and* remote both moved) is **not** merged, pushed or
//!    touched: it is handed off as [`SyncOutcome::Diverged`] for the conflict
//!    layer to reconcile. Nothing is ever force-pushed and no page is ever
//!    auto-merged (see ADR-0010).
//!
//! All git access flows through the audited [`GitRepo`]; this crate never shells
//! out to git itself, so the engine's "destructive operations are
//! unrepresentable" guarantee still bounds every behaviour here.

use std::path::Path;

use berrywiki_core::{PageGraph, WikiPage};
use berrywiki_git::{
    CommitId, Divergence, GitError, GitRepo, Identity, IntegrateOutcome, PushOutcome, Status,
};
use berrywiki_store::{
    Attachment, CreatePageInput, LocalFolderStore, MovePageInput, PageSummary, StoreError,
    WikiStore,
};

/// Commit message used when the precondition flushes pre-existing pending work.
const CHECKPOINT_MSG: &str = "Record changes made outside BerryWiki";

/// Recommended `.gitignore` content for a BerryWiki-managed clone: it keeps the
/// store's atomic-write temp files (`<name>.tmp-<pid>`) from ever being
/// committed. Applied on request via [`SyncedStore::ensure_ignore`].
pub const RECOMMENDED_GITIGNORE: &str = "# BerryWiki atomic-write temp files\n*.tmp-*\n";

/// A failure in the store, in git, or in the wiring's own preconditions.
#[derive(Debug)]
pub enum SyncError {
    Store(StoreError),
    Git(GitError),
    /// A merge is in progress in the clone (unmerged paths). Committing would
    /// bury the conflict markers as if resolved, so we refuse.
    UnmergedPaths { paths: Vec<String> },
    /// `HEAD` is detached; a commit would be unreachable and eventually lost.
    DetachedHead,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Store(e) => write!(f, "{e}"),
            SyncError::Git(e) => write!(f, "{e}"),
            SyncError::UnmergedPaths { paths } => write!(
                f,
                "the wiki clone has an unresolved merge in progress ({} path(s), e.g. {}); \
                 resolve or abort it before saving — your local work is untouched",
                paths.len(),
                paths.first().map(String::as_str).unwrap_or("?"),
            ),
            SyncError::DetachedHead => write!(
                f,
                "the wiki clone's HEAD is not on a branch (detached); a commit here would be \
                 unreachable, so BerryWiki refused it and changed nothing — switch to a branch first",
            ),
        }
    }
}

impl std::error::Error for SyncError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SyncError::Store(e) => Some(e),
            SyncError::Git(e) => Some(e),
            _ => None,
        }
    }
}

impl From<StoreError> for SyncError {
    fn from(e: StoreError) -> Self {
        SyncError::Store(e)
    }
}

impl From<GitError> for SyncError {
    fn from(e: GitError) -> Self {
        SyncError::Git(e)
    }
}

/// Sync-layer result.
pub type Result<T> = std::result::Result<T, SyncError>;

/// What one committing mutation produced.
#[derive(Debug, Clone)]
pub struct Saved<T> {
    /// The wrapped store operation's own return (page id, `()`, attachment…).
    pub value: T,
    /// `Some` when pre-existing pending work was flushed as its own commit
    /// *before* this operation; `None` when the tree was already clean.
    pub checkpoint: Option<CommitId>,
    /// This operation's commit. `None` only when the store wrote byte-identical
    /// content, so there was nothing to commit (never an empty commit).
    pub commit: Option<CommitId>,
}

/// Divergence after a fetch, plus any pending-work checkpoint that fetch made.
#[derive(Debug, Clone)]
pub struct Refreshed {
    pub checkpoint: Option<CommitId>,
    pub divergence: Divergence,
}

/// The result of a full sync pass.
#[derive(Debug, Clone)]
pub struct SyncReport {
    pub checkpoint: Option<CommitId>,
    pub outcome: SyncOutcome,
}

/// The outcome of acting on the current (fetched) divergence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// No upstream configured — commits stay local; nothing to synchronise.
    NoRemote,
    /// Local and fetched remote already agree.
    UpToDate,
    /// Fast-forwarded local onto `fetched` remote commits (no merge, no rewind).
    Integrated { fetched: usize },
    /// Published `pushed` local commits; the remote fast-forwarded.
    Published { pushed: usize },
    /// Local **and** remote both moved. No merge, no fast-forward, no push, no
    /// page touched, sidebar not auto-resolved: the tree is clean and at the
    /// local tip. Handed to the conflict layer via the snapshot.
    Diverged(DivergedHandoff),
    /// The remote advanced between our fetch and our push. Nothing was forced or
    /// lost; the remote is untouched. Re-syncing re-fetches and reclassifies
    /// (almost always to [`SyncOutcome::Diverged`]).
    PushRaced,
}

/// An immutable snapshot of a diverged history for the conflict layer. Every
/// field references an already-fetched commit, so reconciliation is fully
/// offline and deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivergedHandoff {
    /// Our `HEAD` at hand-off; the working tree matches it exactly.
    pub local: CommitId,
    /// The fetched upstream tip.
    pub upstream: CommitId,
    /// The best common ancestor of `local` and `upstream` — the three-way base.
    pub base: CommitId,
    /// Local commits since `base`.
    pub ahead: usize,
    /// Upstream commits since `base`.
    pub behind: usize,
}

/// A wiki whose saves are committed and whose commits can be synchronised.
pub struct SyncedStore<S: WikiStore = LocalFolderStore> {
    store: S,
    git: GitRepo,
}

impl SyncedStore<LocalFolderStore> {
    /// Open a local clone as a synced wiki: a [`LocalFolderStore`] and a
    /// [`GitRepo`] over the same canonical directory, committing as `identity`.
    ///
    /// For a credentialed remote, build the [`GitRepo`] yourself
    /// (`GitRepo::open(root)?.with_identity(..).with_env("GIT_ASKPASS", ..)`)
    /// and use [`SyncedStore::new`].
    pub fn open_local(path: impl AsRef<Path>, identity: Identity) -> Result<Self> {
        let store = LocalFolderStore::open(path.as_ref())?;
        let git = GitRepo::open(store.root())?.with_identity(identity);
        Ok(SyncedStore { store, git })
    }
}

impl<S: WikiStore> SyncedStore<S> {
    /// Wire a store and a git handle that already point at the same working
    /// tree. The caller guarantees that alignment.
    pub fn new(store: S, git: GitRepo) -> Self {
        SyncedStore { store, git }
    }

    // ----- read-through -----

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn graph(&self) -> &PageGraph {
        self.store.graph()
    }

    pub fn list_pages(&self) -> Vec<PageSummary> {
        self.store.list_pages()
    }

    pub fn read_page(&self, id: &str) -> Result<&WikiPage> {
        Ok(self.store.read_page(id)?)
    }

    /// Pending working-tree changes right now (does not fetch).
    pub fn status(&self) -> Result<Status> {
        Ok(self.git.status()?)
    }

    /// Divergence against the *last-fetched* upstream (does not fetch).
    pub fn divergence(&self) -> Result<Divergence> {
        Ok(self.git.divergence()?)
    }

    // ----- commit-on-save: each call is exactly one atomic logical commit -----

    pub fn create_page(&mut self, input: CreatePageInput) -> Result<Saved<String>> {
        let checkpoint = self.ensure_committable()?;
        let title = input.title.clone();
        let id = self.store.create_page(input)?;
        let commit = self.git.commit_all(&msg_create(&title))?;
        Ok(Saved { value: id, checkpoint, commit })
    }

    pub fn update_page(&mut self, id: &str, new_body: &str) -> Result<Saved<()>> {
        let checkpoint = self.ensure_committable()?;
        self.store.update_page(id, new_body)?;
        // The body's H1 may have changed the title; read it back for the message.
        let title = self.store.read_page(id)?.title.clone();
        let commit = self.git.commit_all(&msg_update(&title))?;
        Ok(Saved { value: (), checkpoint, commit })
    }

    pub fn move_page(&mut self, input: MovePageInput) -> Result<Saved<()>> {
        let checkpoint = self.ensure_committable()?;
        let id = input.id.clone();
        self.store.move_page(input)?;
        let title = self.store.read_page(&id)?.title.clone();
        let commit = self.git.commit_all(&msg_move(&title))?;
        Ok(Saved { value: (), checkpoint, commit })
    }

    pub fn delete_page(&mut self, id: &str) -> Result<Saved<()>> {
        let checkpoint = self.ensure_committable()?;
        // Capture the title before the page leaves the graph.
        let title = self.store.read_page(id)?.title.clone();
        self.store.delete_page(id)?;
        let commit = self.git.commit_all(&msg_delete(&title))?;
        Ok(Saved { value: (), checkpoint, commit })
    }

    pub fn add_attachment(
        &mut self,
        page_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<Saved<Attachment>> {
        let checkpoint = self.ensure_committable()?;
        let title = self.store.read_page(page_id)?.title.clone();
        let attachment = self.store.add_attachment(page_id, filename, bytes)?;
        let commit = self.git.commit_all(&msg_attach(filename, &title))?;
        Ok(Saved { value: attachment, checkpoint, commit })
    }

    // ----- the remote cycle -----

    /// Precondition + fetch, returning the up-to-date divergence. A repo with no
    /// upstream is not fetched (there is nothing to fetch from).
    pub fn refresh(&mut self) -> Result<Refreshed> {
        let checkpoint = self.ensure_committable()?;
        let divergence = self.git.divergence()?;
        if !divergence.has_upstream {
            return Ok(Refreshed { checkpoint, divergence });
        }
        // Fetch before push (non-negotiable): the working tree is untouched.
        self.git.fetch()?;
        Ok(Refreshed { checkpoint, divergence: self.git.divergence()? })
    }

    /// Act on the *current* (already-fetched) divergence — no re-fetch, so only
    /// [`SyncedStore::push`]-equivalent publishing can race.
    pub fn advance(&mut self) -> Result<SyncOutcome> {
        let d = self.git.divergence()?;
        if !d.has_upstream {
            return Ok(SyncOutcome::NoRemote);
        }
        if d.is_up_to_date() {
            return Ok(SyncOutcome::UpToDate);
        }
        if d.can_fast_forward() {
            return Ok(match self.git.fast_forward_to_upstream()? {
                // The only branch that changes the working tree, so the only one
                // that reloads the store's graph.
                IntegrateOutcome::FastForwarded => {
                    self.store.reload()?;
                    SyncOutcome::Integrated { fetched: d.behind }
                }
                IntegrateOutcome::AlreadyUpToDate => SyncOutcome::UpToDate,
                IntegrateOutcome::NeedsManualMerge => SyncOutcome::Diverged(self.handoff(&d)?),
                IntegrateOutcome::NoUpstream => SyncOutcome::NoRemote,
            });
        }
        if d.can_publish() {
            return Ok(match self.git.push()? {
                PushOutcome::Pushed => SyncOutcome::Published { pushed: d.ahead },
                PushOutcome::UpToDate => SyncOutcome::UpToDate,
                PushOutcome::RejectedNonFastForward => SyncOutcome::PushRaced,
                PushOutcome::NoUpstream => SyncOutcome::NoRemote,
            });
        }
        // ahead > 0 && behind > 0: genuinely diverged. No merge, ff or push.
        Ok(SyncOutcome::Diverged(self.handoff(&d)?))
    }

    /// One full pass: refresh then advance.
    pub fn sync(&mut self) -> Result<SyncReport> {
        let refreshed = self.refresh()?;
        let outcome = self.advance()?;
        Ok(SyncReport { checkpoint: refreshed.checkpoint, outcome })
    }

    /// Re-run [`SyncedStore::sync`] while the outcome is [`SyncOutcome::PushRaced`],
    /// up to `max_attempts` times. A race means the remote gained commits we
    /// lack, so the next fetch makes us `behind` and (being `ahead`) the pass
    /// reclassifies to [`SyncOutcome::Diverged`] — terminal. Never force-pushes.
    pub fn sync_retrying(&mut self, max_attempts: u32) -> Result<SyncReport> {
        let attempts = max_attempts.max(1);
        let mut last = None;
        for _ in 0..attempts {
            let report = self.sync()?;
            if matches!(report.outcome, SyncOutcome::PushRaced) {
                last = Some(report);
                continue;
            }
            return Ok(report);
        }
        Ok(last.expect("at least one attempt always runs"))
    }

    // ----- conflict re-entry seam (used by the conflict layer, not the UI) -----

    /// Read-only git handle (for three-way reads and a controlled merge).
    pub fn git(&self) -> &GitRepo {
        &self.git
    }

    /// Direct store access, bypassing commit-on-save. Used to write resolved
    /// pages and regenerate the sidebar during conflict resolution; follow with
    /// [`SyncedStore::commit`].
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Rebuild the in-memory graph from disk.
    pub fn reload(&mut self) -> Result<()> {
        Ok(self.store.reload()?)
    }

    /// Commit whatever is currently pending with an explicit message. Not
    /// precondition-gated: the conflict layer uses it to record a resolution
    /// (its `add -A` lands the regenerated sidebar in the same commit).
    pub fn commit(&mut self, message: &str) -> Result<Option<CommitId>> {
        Ok(self.git.commit_all(message)?)
    }

    /// Ensure the clone ignores the store's atomic-write temp files, committing
    /// a `.gitignore` update when needed. Opt-in setup; call once on a clone.
    pub fn ensure_ignore(&mut self) -> Result<Option<CommitId>> {
        let path = self.git.workdir().join(".gitignore");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        if existing.lines().any(|l| l.trim() == "*.tmp-*") {
            return Ok(None);
        }
        // Flush any unrelated pending work first so the ignore rule is its own
        // clean commit.
        self.ensure_committable()?;
        let mut content = existing;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(RECOMMENDED_GITIGNORE);
        std::fs::write(&path, content).map_err(|e| SyncError::Git(GitError::Io(e)))?;
        self.commit("Configure BerryWiki ignore rules")
    }

    // ----- internals -----

    /// Make the tree committable for a fresh logical commit. On a clean tree,
    /// `Ok(None)`. Ordinary pending work is flushed as its **own** checkpoint
    /// commit (never discarded, never folded into the next op). Two states are
    /// refused because committing them would silently lose or corrupt work: an
    /// in-progress merge (unmerged paths) and a detached `HEAD`.
    ///
    /// Deliberately does **not** reload the store: keeping its stale-write
    /// fingerprints from the last load means a later mutation to a page that was
    /// edited outside BerryWiki still trips `StaleWrite` rather than clobbering
    /// the foreign edit (which is, by then, safely committed).
    fn ensure_committable(&mut self) -> Result<Option<CommitId>> {
        // Checked FIRST and unconditionally: every mutation ends in a commit, so
        // even on a clean detached HEAD the commit we are about to make would be
        // unreachable. Refuse before touching anything.
        if self.git.current_branch()?.is_none() {
            return Err(SyncError::DetachedHead);
        }
        let status = self.git.status()?;
        if status.is_clean() {
            return Ok(None);
        }
        let unmerged: Vec<String> = status
            .entries
            .iter()
            .filter(|e| is_unmerged(e))
            .cloned()
            .collect();
        if !unmerged.is_empty() {
            return Err(SyncError::UnmergedPaths { paths: unmerged });
        }
        Ok(self.git.commit_all(CHECKPOINT_MSG)?)
    }

    fn handoff(&self, d: &Divergence) -> Result<DivergedHandoff> {
        Ok(DivergedHandoff {
            local: self.git.head()?,
            upstream: self.git.head_of_upstream()?,
            base: self.git.merge_base_with_upstream()?,
            ahead: d.ahead,
            behind: d.behind,
        })
    }
}

/// Is a porcelain `XY path` entry an unmerged (conflict) state?
fn is_unmerged(entry: &str) -> bool {
    let b = entry.as_bytes();
    if b.len() < 2 {
        return false;
    }
    let (x, y) = (b[0], b[1]);
    x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D')
}

fn msg_create(title: &str) -> String {
    format!("Create page \"{}\"", sanitise_subject(title))
}
fn msg_update(title: &str) -> String {
    format!("Update page \"{}\"", sanitise_subject(title))
}
fn msg_move(title: &str) -> String {
    format!("Move page \"{}\"", sanitise_subject(title))
}
fn msg_delete(title: &str) -> String {
    format!("Delete page \"{}\"", sanitise_subject(title))
}
fn msg_attach(filename: &str, title: &str) -> String {
    format!(
        "Attach \"{}\" to \"{}\"",
        sanitise_subject(filename),
        sanitise_subject(title)
    )
}

/// Fold a (possibly untrusted) title/filename into a single clean commit-subject
/// fragment: control chars become spaces, runs of whitespace collapse, and the
/// result is trimmed and length-capped (char-safe). Pure and deterministic.
fn sanitise_subject(s: &str) -> String {
    let mut out = String::new();
    let mut prev_space = false;
    for ch in s.chars() {
        let c = if ch.is_control() { ' ' } else { ch };
        if c == ' ' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    let trimmed = out.trim();
    if trimmed.chars().count() > 72 {
        let capped: String = trimmed.chars().take(71).collect();
        format!("{capped}…")
    } else {
        trimmed.to_string()
    }
}
