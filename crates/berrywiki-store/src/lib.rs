// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! # berrywiki-store
//!
//! Storage adapters for BerryWiki. All repository access goes through the
//! [`WikiStore`] trait so the engine and (later) the UI never touch the
//! filesystem, git or GitHub directly.
//!
//! Phase 0 ships [`LocalFolderStore`]: a plain-directory adapter that lets the
//! whole product be developed and tested without live GitHub credentials.
//! `GitHubWikiStore` will implement the same trait later (credential-gated;
//! never faked).
//!
//! Safety properties honoured here (spec non-negotiables):
//! * Writes are atomic: content is written to a temp file in the target
//!   directory and renamed into place, so no reader ever sees a partial page.
//! * Path traversal is impossible: every filename is validated component-wise
//!   and every resolved path is checked to remain under the store root.
//! * A move never destroys content: the new file is written and verified
//!   before the old file is removed; a crash in between leaves *both* files
//!   (surfaced as a duplicate-id diagnostic), never neither.
//! * The generated `_Sidebar.md` is only rewritten when its content changes.

pub mod error;
pub mod local;
pub mod paths;

pub use error::StoreError;
pub use local::{LocalFolderStore, SIDEBAR_FILE};

use berrywiki_core::{PageGraph, PageKind, WikiPage};

/// Result alias for store operations.
pub type Result<T> = std::result::Result<T, StoreError>;

/// A cheap listing of a page, without its full body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSummary {
    pub id: String,
    pub title: String,
    pub path: String,
    pub archived: bool,
}

/// Input for creating a new managed page.
///
/// The caller supplies the `id` (the application layer owns id generation —
/// UUIDv7 in production, deterministic ids in tests). The store validates
/// uniqueness.
#[derive(Debug, Clone)]
pub struct CreatePageInput {
    pub id: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub position: i64,
    pub kind: PageKind,
    pub tags: Vec<String>,
    /// Markdown body. A `# Title` heading is prepended when absent.
    pub body: String,
    /// Extra metadata lines to write verbatim into the page's metadata
    /// block, in the `key: value` form the parser preserves under
    /// [`PageMetadata::extra`].
    ///
    /// This exists so a caller that mints a page on another system's behalf
    /// can record where it came from. The importer writes `source:` here,
    /// which is what lets a second import recognise its own earlier work
    /// instead of duplicating it. Ordinary callers pass an empty vector.
    pub extra: Vec<String>,
}

/// Input for moving/re-parenting a page.
#[derive(Debug, Clone)]
pub struct MovePageInput {
    pub id: String,
    /// `None` makes the page a root.
    pub new_parent_id: Option<String>,
    pub new_position: i64,
}

/// One filename change a move would make (the moved page or a descendant,
/// since filenames encode ancestry — ADR-0001).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRename {
    pub id: String,
    pub title: String,
    pub old_path: String,
    pub new_path: String,
}

/// One page *outside* the moved subtree whose inbound links a move would
/// rewrite in place (its own filename is unchanged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRewrite {
    pub id: String,
    pub title: String,
    pub path: String,
}

/// The dry run of a move: everything [`LocalFolderStore::plan_move`] decided
/// without touching the wiki folder. The public fields are the summary a UI
/// can show; the crate-private fields are the exact staged writes, deletes
/// and journal entries that `apply_move` executes, so what a preview lists
/// and what the move does cannot drift apart.
///
/// A plan is advisory: the store re-plans against the live graph on the real
/// move, so a plan is never serialised, handed back, or applied later.
#[derive(Debug, Clone)]
pub struct MovePlan {
    pub id: String,
    pub new_parent_id: Option<String>,
    pub new_position: i64,
    /// Filename changes, moved page first, then descendants in tree order.
    pub renames: Vec<PlannedRename>,
    /// Pages outside the subtree whose links are rewritten in place.
    pub link_rewrites: Vec<PlannedRewrite>,
    pub(crate) writes: Vec<(String, Vec<u8>)>,
    pub(crate) deletes: Vec<String>,
    pub(crate) journal: Vec<berrywiki_appstate::journal::RenameEntry>,
}

/// A stored attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub page_id: String,
    pub filename: String,
    /// Repository-relative path, e.g. `assets/<page-id>/<filename>`.
    pub path: String,
}

/// The storage abstraction every adapter implements.
///
/// Synchronous by design for Phase 0: the local adapter is pure filesystem
/// work and the engine is CPU-bound. If the GitHub adapter needs async I/O it
/// will wrap this trait rather than force async through the whole stack —
/// revisit in the implementation plan if evidence demands it.
pub trait WikiStore {
    /// Rebuild the in-memory graph from storage. Called automatically after
    /// each mutation; callable manually after external edits.
    fn reload(&mut self) -> Result<()>;

    /// The current derived graph (pages, hierarchy, backlinks, diagnostics).
    fn graph(&self) -> &PageGraph;

    fn list_pages(&self) -> Vec<PageSummary>;

    fn read_page(&self, id: &str) -> Result<&WikiPage>;

    /// Create a managed page. Returns the page id.
    fn create_page(&mut self, input: CreatePageInput) -> Result<String>;

    /// Replace a page's body, preserving its metadata byte-stably.
    fn update_page(&mut self, id: &str, new_body: &str) -> Result<()>;

    /// Replace a page's body *and* its tag list in one write, preserving all
    /// other metadata byte-stably.
    ///
    /// Deliberately separate from [`WikiStore::update_page`] rather than the
    /// path it delegates to: a plain body save must never rewrite a
    /// hand-authored tag list (duplicates, ordering and spacing included),
    /// and a caller that has no opinion about tags must not be able to clear
    /// them by omission.
    ///
    /// Tags are *validated, never normalised*. A tag that would not survive a
    /// serialise/parse round trip — empty, untrimmed, or carrying a newline,
    /// a control character or `-->` — is refused with
    /// [`StoreError::InvalidTag`] and nothing is written.
    fn update_page_with_tags(&mut self, id: &str, new_body: &str, tags: &[String]) -> Result<()>;

    /// Re-parent and re-order a page (metadata + filename + sidebar together).
    fn move_page(&mut self, input: MovePageInput) -> Result<()>;

    /// Delete a page. Refuses when the page still has children.
    fn delete_page(&mut self, id: &str) -> Result<()>;

    /// Store an attachment under `assets/<page-id>/`.
    fn add_attachment(&mut self, page_id: &str, filename: &str, bytes: &[u8])
        -> Result<Attachment>;

    /// List a page's attachments, sorted by filename.
    ///
    /// A page with no `assets/<page-id>/` directory has no attachments; that
    /// is an empty list, not an error. Entries whose names would not survive
    /// [`validate_component`](crate::paths::validate_component) are skipped
    /// rather than reported, so a file planted by hand cannot be served.
    fn attachments(&self, page_id: &str) -> Result<Vec<Attachment>>;

    /// Read one attachment's bytes.
    fn read_attachment(&self, page_id: &str, filename: &str) -> Result<Vec<u8>>;

    /// Regenerate `_Sidebar.md`. Returns `true` when the file was written,
    /// `false` when the existing content was already up to date.
    fn regenerate_sidebar(&mut self) -> Result<bool>;
}
