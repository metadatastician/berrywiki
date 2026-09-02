// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Conflict classification for a merge someone started in the wiki folder.
//!
//! BerryWiki never begins a merge (ADR-0010): [`crate::SyncedStore::sync`]
//! hands a diverged history back untouched. What this module adds is the
//! second half of that hand-off — once a person has run `git merge` themselves,
//! it reads git's own record of the conflict out of the index and says, per
//! path, *what kind* of disagreement it is and what each of the three sides
//! holds.
//!
//! The classification is exact rather than predicted, because it comes from the
//! stages git wrote when it tried the merge: stage 1 is the common ancestor,
//! stage 2 the local side, stage 3 the incoming side. Which stages exist
//! distinguishes an edit/edit from an add/add or an edit/delete, and reading
//! stages 2 and 3 back distinguishes a genuine body disagreement from two
//! sides that changed only the metadata block.
//!
//! Exactly one kind is resolved automatically. `_Sidebar.md` is generated
//! output, never authored, so a conflict in it is not a disagreement between
//! two people — it is two derivations of a tree that has since changed again.
//! Regenerating it from the merged pages is the only correct answer, and it is
//! what [`crate::SyncedStore::finish_merge`] does. Every other conflict is
//! surfaced verbatim for a person to settle; nothing is merged, unioned or
//! guessed, metadata blocks least of all.

use berrywiki_core::parse_source;
use berrywiki_git::{CommitId, Stage, UnmergedEntry};
use berrywiki_store::{WikiStore, SIDEBAR_FILE};

use crate::{Result, SyncError, SyncedStore};

/// What kind of disagreement git recorded for one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// The generated sidebar. Resolvable by regeneration, and only that way.
    Sidebar,
    /// Both sides changed the page's body text.
    Body,
    /// Both sides changed the page, but only inside the metadata block — the
    /// body text is identical. Surfaced, never unioned: two metadata blocks
    /// merged field-by-field can express a tree neither author wrote.
    Metadata,
    /// Neither side started from a common ancestor: both created this path.
    AddedBoth,
    /// The local side kept the page; the incoming side removed it.
    DeletedByThem,
    /// The incoming side kept the page; the local side removed it.
    DeletedByUs,
    /// Not a Markdown page (an attachment, say). Content is not read, because
    /// it may not be text at all.
    Opaque,
}

impl ConflictKind {
    /// Can BerryWiki settle this on its own? True only for the generated
    /// sidebar, which is derived from the merged pages rather than authored.
    pub fn is_auto_resolvable(self) -> bool {
        matches!(self, ConflictKind::Sidebar)
    }

    /// A fixed, translatable-free description for the conflicts page.
    pub fn describe(self) -> &'static str {
        match self {
            ConflictKind::Sidebar => {
                "Generated navigation. BerryWiki regenerates it from the merged pages."
            }
            ConflictKind::Body => "Both sides changed the page text.",
            ConflictKind::Metadata => {
                "Both sides changed the metadata block; the page text is identical."
            }
            ConflictKind::AddedBoth => "Both sides created this file independently.",
            ConflictKind::DeletedByThem => "Changed here, removed on the remote.",
            ConflictKind::DeletedByUs => "Removed here, changed on the remote.",
            ConflictKind::Opaque => "Not a Markdown page; contents not shown.",
        }
    }
}

/// One conflicted path, classified, with whatever each side holds.
///
/// `base`, `ours` and `theirs` are `None` when that stage does not exist (the
/// side removed the file, or never had it) and also for [`ConflictKind::Opaque`]
/// paths, whose bytes are deliberately not decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileConflict {
    pub path: String,
    pub kind: ConflictKind,
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
}

/// Every conflict in the merge currently in progress.
///
/// An empty `files` is meaningful and not an error: it is a merge whose
/// conflicts a person has already settled in their editor but not yet
/// concluded with a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictReport {
    pub files: Vec<FileConflict>,
}

impl ConflictReport {
    /// The conflicts BerryWiki will not settle for anyone: everything except
    /// the generated sidebar.
    pub fn blocking(&self) -> impl Iterator<Item = &FileConflict> {
        self.files.iter().filter(|f| !f.kind.is_auto_resolvable())
    }

    /// Paths a person still has to settle, for the refusal message.
    pub fn blocking_paths(&self) -> Vec<String> {
        self.blocking().map(|f| f.path.clone()).collect()
    }

    /// May the merge be concluded? True when nothing is left that a person has
    /// to decide — either everything is settled already, or the only thing
    /// outstanding is the generated sidebar.
    pub fn can_finish(&self) -> bool {
        self.blocking().next().is_none()
    }

    /// The sidebar is the only thing left outstanding.
    pub fn is_sidebar_only(&self) -> bool {
        !self.files.is_empty() && self.can_finish()
    }
}

/// Is this a page file, whose two sides can be compared as text?
fn is_page(path: &str) -> bool {
    path.ends_with(".md")
}

/// Classify one conflicted path from the stages git recorded plus, for a page,
/// what the two sides hold.
///
/// Pure so the decision table is testable without a repository.
pub(crate) fn classify(
    entry: &UnmergedEntry,
    ours: Option<&str>,
    theirs: Option<&str>,
) -> ConflictKind {
    if entry.path == SIDEBAR_FILE {
        return ConflictKind::Sidebar;
    }
    if !is_page(&entry.path) {
        return ConflictKind::Opaque;
    }
    if !entry.base {
        return ConflictKind::AddedBoth;
    }
    if !entry.theirs {
        return ConflictKind::DeletedByThem;
    }
    if !entry.ours {
        return ConflictKind::DeletedByUs;
    }
    match (ours, theirs) {
        // Same body text on both sides: the disagreement is confined to the
        // metadata block. Named separately because merging two metadata blocks
        // is exactly the thing this crate must never do.
        (Some(o), Some(t)) if parse_source(o).body == parse_source(t).body => {
            ConflictKind::Metadata
        }
        _ => ConflictKind::Body,
    }
}

/// The message on the commit that concludes a merge. Fixed, so history stays
/// legible and the tests can assert it.
pub(crate) const MERGE_MSG: &str = "Merge remote changes into the wiki";

impl<S: WikiStore> SyncedStore<S> {
    /// The conflicts in the merge currently in progress, or `None` when no
    /// merge is under way.
    ///
    /// Reads only: the index, and the blobs it already holds. Nothing in the
    /// working tree is touched, so calling this can neither settle nor worsen
    /// the conflict.
    pub fn conflicts(&self) -> Result<Option<ConflictReport>> {
        if !self.git.merge_in_progress()? {
            return Ok(None);
        }
        let mut files = Vec::new();
        for entry in self.git.unmerged()? {
            // Stage contents are read only for Markdown pages: an attachment
            // need not be text at all, and lossily decoding it would be a lie.
            let readable = is_page(&entry.path);
            let read = |stage, present: bool| -> Result<Option<String>> {
                if !present || !readable {
                    return Ok(None);
                }
                Ok(self.git.show_stage(stage, &entry.path)?)
            };
            let base = read(Stage::Base, entry.base)?;
            let ours = read(Stage::Ours, entry.ours)?;
            let theirs = read(Stage::Theirs, entry.theirs)?;
            let kind = classify(&entry, ours.as_deref(), theirs.as_deref());
            files.push(FileConflict {
                path: entry.path,
                kind,
                base,
                ours,
                theirs,
            });
        }
        Ok(Some(ConflictReport { files }))
    }

    /// Conclude a merge a person started, once nothing is left for them to
    /// decide.
    ///
    /// Refuses with [`SyncError::UnmergedPaths`] while any page is still
    /// unsettled, so conflict markers can never be committed as if they were
    /// content, and with [`SyncError::NoMergeInProgress`] when there is
    /// no merge to conclude.
    ///
    /// Otherwise, in this order: reload the store, so the graph is the *merged*
    /// tree rather than the one from before the merge; regenerate the sidebar
    /// from it, which both settles a sidebar conflict and keeps the generated
    /// navigation consistent with a tree that just changed (INV-3); then commit
    /// the merge, sidebar included, as one commit with both parents (INV-4 —
    /// nothing is discarded and no history is replaced).
    pub fn finish_merge(&mut self) -> Result<CommitId> {
        let Some(report) = self.conflicts()? else {
            return Err(SyncError::NoMergeInProgress);
        };
        let blocking = report.blocking_paths();
        if !blocking.is_empty() {
            return Err(SyncError::UnmergedPaths { paths: blocking });
        }
        // The merge put other people's pages in the working tree; the in-memory
        // graph predates them. Regenerating before this reload would write the
        // *pre-merge* sidebar into the merge commit.
        self.store.reload()?;
        self.store.regenerate_sidebar()?;
        Ok(self.git.commit_merge(MERGE_MSG)?)
    }
}
