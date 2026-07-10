//! Actionable store errors.
//!
//! Spec requirement: every failure says what was attempted, whether local work
//! is safe, and what to do next. These variants carry that context; adapters
//! must never stringify tokens or absolute paths outside the wiki root into
//! them.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum StoreError {
    /// The store root does not exist or is not a directory.
    RootNotFound(String),
    /// No page with the given id.
    PageNotFound(String),
    /// A page with this id already exists.
    DuplicateId(String),
    /// The referenced parent page does not exist.
    ParentNotFound(String),
    /// The referenced parent has no stable id (unmanaged), so it cannot own
    /// children — its filename is not a durable identifier.
    UnmanagedParent(String),
    /// Two pages on disk share this id; a mutation would act on an arbitrary
    /// one. Resolve the duplicate before mutating.
    AmbiguousId(String),
    /// The move would make a page its own ancestor.
    CycleDetected { page: String, parent: String },
    /// The page has no BerryWiki metadata, so it cannot be managed
    /// (moved/re-parented) until an id is assigned.
    UnmanagedPage(String),
    /// The page still has children; delete or re-parent them first.
    HasChildren { page: String, child_count: usize },
    /// A filename or title failed validation (traversal, reserved or
    /// filesystem-hostile characters, emptiness).
    InvalidName { name: String, reason: String },
    /// An attachment with this filename already exists for the page.
    DuplicateAttachment { page: String, filename: String },
    /// Underlying I/O failure. Local work on other pages is unaffected.
    Io { context: String, source: io::Error },
}

impl StoreError {
    pub(crate) fn io(context: impl Into<String>, source: io::Error) -> Self {
        StoreError::Io {
            context: context.into(),
            source,
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::RootNotFound(p) => write!(
                f,
                "Wiki folder {p:?} was not found or is not a directory. \
                 Nothing was changed."
            ),
            StoreError::PageNotFound(id) => write!(
                f,
                "No page with id {id:?}. Nothing was changed; reload and retry."
            ),
            StoreError::DuplicateId(id) => write!(
                f,
                "A page with id {id:?} already exists. Nothing was changed; \
                 use a fresh id."
            ),
            StoreError::ParentNotFound(id) => write!(
                f,
                "Parent page {id:?} does not exist. Nothing was changed."
            ),
            StoreError::UnmanagedParent(id) => write!(
                f,
                "Page {id:?} has no BerryWiki metadata, so it cannot be used as \
                 a parent — its filename is not a stable id. Give it metadata \
                 first. Nothing was changed."
            ),
            StoreError::AmbiguousId(id) => write!(
                f,
                "Two pages on disk share id {id:?}; a change would act on an \
                 unpredictable one. Nothing was changed — resolve the duplicate \
                 (often a leftover from an interrupted move) first."
            ),
            StoreError::CycleDetected { page, parent } => write!(
                f,
                "Moving page {page:?} under {parent:?} would make it its own \
                 ancestor. The move was refused; nothing was changed."
            ),
            StoreError::UnmanagedPage(id) => write!(
                f,
                "Page {id:?} has no BerryWiki metadata so it cannot be moved or \
                 re-parented. Assign it metadata first (a create/adopt step the \
                 caller must perform); nothing was changed."
            ),
            StoreError::HasChildren { page, child_count } => write!(
                f,
                "Page {page:?} still has {child_count} child page(s). Move or \
                 delete them first; nothing was changed."
            ),
            StoreError::InvalidName { name, reason } => write!(
                f,
                "Name {name:?} was rejected: {reason}. Nothing was changed."
            ),
            StoreError::DuplicateAttachment { page, filename } => write!(
                f,
                "Attachment {filename:?} already exists for page {page:?}. \
                 Nothing was overwritten; rename the file and retry."
            ),
            StoreError::Io { context, source } => write!(
                f,
                "I/O failure while {context}: {source}. Files already on disk \
                 are intact; retry after resolving the underlying cause."
            ),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
