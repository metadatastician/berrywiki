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
pub use local::LocalFolderStore;

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
}

/// Input for moving/re-parenting a page.
#[derive(Debug, Clone)]
pub struct MovePageInput {
    pub id: String,
    /// `None` makes the page a root.
    pub new_parent_id: Option<String>,
    pub new_position: i64,
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

    /// Re-parent and re-order a page (metadata + filename + sidebar together).
    fn move_page(&mut self, input: MovePageInput) -> Result<()>;

    /// Delete a page. Refuses when the page still has children.
    fn delete_page(&mut self, id: &str) -> Result<()>;

    /// Store an attachment under `assets/<page-id>/`.
    fn add_attachment(&mut self, page_id: &str, filename: &str, bytes: &[u8]) -> Result<Attachment>;

    /// Regenerate `_Sidebar.md`. Returns `true` when the file was written,
    /// `false` when the existing content was already up to date.
    fn regenerate_sidebar(&mut self) -> Result<bool>;
}
