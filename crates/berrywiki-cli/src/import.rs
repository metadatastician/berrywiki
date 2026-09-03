// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! `berrywiki import <notebook> <wiki>` — bring a CherryTree notebook in.
//!
//! # Dry run is the default
//!
//! Running this command without `--apply` writes nothing. It reads the
//! notebook, works out what it *would* do, and prints a report saying what
//! would be created, what is already there, and what could not be carried
//! across. An import is a bulk write into somebody's notes, and the reversible
//! choice is to make them ask for it twice.
//!
//! # What it refuses, and why refusing is the point
//!
//! `--apply` stops before touching anything if:
//!
//! * two siblings would share a title, because filenames encode ancestry
//!   (ADR-0001) and the second write would land on a disambiguated name that
//!   nobody chose;
//! * a page id already exists but was **not** made by importing this same node
//!   from this same file, because that page is somebody's work and overwriting
//!   it is not this command's business;
//! * the wiki is not a git working tree, because an import that cannot be
//!   committed also cannot be undone with one command.
//!
//! There is deliberately no `--overwrite`. Refusing and naming the page is a
//! decision the user can act on; a flag that clobbers on request is one they
//! can regret, and nothing here needs it yet.
//!
//! # Importing twice is importing once
//!
//! Every page carries a `source:` line in its metadata naming the file hash
//! and the node it came from ([`berrywiki_import::marker`]), and page ids are
//! derived from that same marker. So a second run of the same notebook against
//! the same wiki finds every page already present and correctly attributed,
//! reports them as unchanged, and writes nothing — including when the user has
//! since edited the bodies, which are left exactly as they are.
//!
//! # One commit
//!
//! Pages are written through the store directly rather than through
//! [`berrywiki_sync::SyncedStore`]'s committing methods, and a single
//! [`berrywiki_sync::SyncedStore::commit`] records the whole import. An import
//! is one act; it should be one thing to revert.

use std::io::{self, Write};
use std::path::Path;

use berrywiki_core::PageKind;
use berrywiki_import::{report, ImportModel, Mode, Run};
use berrywiki_store::{CreatePageInput, LocalFolderStore, WikiStore};

use crate::{has_flag, writer_lock, WriterLock};

const USAGE: &str = "usage: berrywiki import <notebook.ctd> <wiki-folder> [--apply] [--json]";

/// What an import would do to one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// No page has this id; it will be created.
    Create,
    /// The page exists and carries this node's own marker. Nothing to do.
    Unchanged,
    /// The page exists and is not ours. The run is refused.
    Occupied,
}

/// Read the `source:` marker a page carries, if it carries one.
fn page_marker(store: &LocalFolderStore, id: &str) -> Option<String> {
    let page = store.read_page(id).ok()?;
    let meta = page.metadata.as_ref()?;
    meta.extra
        .iter()
        .find_map(|line| line.strip_prefix("source:"))
        .map(|v| v.trim().to_string())
}

/// Decide, for every node, what the import would do to it.
fn plan(model: &ImportModel, store: &LocalFolderStore) -> Vec<Plan> {
    model
        .nodes
        .iter()
        .map(|n| {
            if store.read_page(&n.id).is_err() {
                Plan::Create
            } else if page_marker(store, &n.id).as_deref() == Some(n.marker.as_str()) {
                Plan::Unchanged
            } else {
                Plan::Occupied
            }
        })
        .collect()
}

/// Print the create/unchanged/occupied tally and name the occupied pages.
///
/// The report proper says what the *notebook* holds; this says what would
/// happen to the *wiki*, which the report cannot know.
fn write_plan(model: &ImportModel, plans: &[Plan], out: &mut dyn Write) -> io::Result<()> {
    let count = |p: Plan| plans.iter().filter(|q| **q == p).count();
    writeln!(
        out,
        "\nPlan against this wiki: {} to create, {} already imported and unchanged, {} occupied \
         by something else.",
        count(Plan::Create),
        count(Plan::Unchanged),
        count(Plan::Occupied),
    )?;
    for (n, p) in model.nodes.iter().zip(plans) {
        if *p == Plan::Occupied {
            writeln!(
                out,
                "  occupied: {} (page id {}) already exists and was not imported from this node",
                n.title, n.id
            )?;
        }
    }
    Ok(())
}

pub(crate) fn cmd_import(rest: &[String], out: &mut dyn Write) -> io::Result<i32> {
    let positional: Vec<&str> = rest
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    if positional.len() != 2 {
        writeln!(out, "{USAGE}")?;
        return Ok(2);
    }
    let source = Path::new(positional[0]);
    let wiki = Path::new(positional[1]);
    let apply = has_flag(rest, "--apply");
    let as_json = has_flag(rest, "--json");

    let bytes = match std::fs::read(source) {
        Ok(b) => b,
        Err(e) => {
            writeln!(out, "error: {}: {e}", source.display())?;
            return Ok(2);
        }
    };

    // Only the file's own name reaches the report. A report is the thing a
    // user pastes into a bug tracker, and the directories above a personal
    // notebook are nobody else's business.
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("notebook");

    let model = match berrywiki_import::import(name, &bytes) {
        Ok(m) => m,
        Err(message) => {
            writeln!(out, "error: {message}")?;
            return Ok(2);
        }
    };

    let store = match LocalFolderStore::open(wiki) {
        Ok(s) => s,
        Err(e) => {
            writeln!(out, "error: {e}")?;
            return Ok(2);
        }
    };

    // Held across the read-plan-write sequence, so the wiki cannot change
    // under us between deciding what to do and doing it. A dry run takes it
    // too: a plan computed against a wiki somebody else is editing is a plan
    // about a wiki that no longer exists.
    let _lock = match writer_lock(&store, "import", &wiki.display().to_string(), out)? {
        WriterLock::Refused => return Ok(2),
        WriterLock::Held(l) => Some(l),
        WriterLock::Unavailable => None,
    };

    let plans = plan(&model, &store);
    let run = Run {
        source: name,
        mode: if apply { Mode::Applied } else { Mode::DryRun },
    };

    if !apply {
        let text = if as_json {
            report::json(&model, &run)
        } else {
            report::asciidoc(&model, &run)
        };
        write!(out, "{text}")?;
        if !as_json {
            write_plan(&model, &plans, out)?;
        }
        return Ok(0);
    }

    // ---- refusals, all before the first write ----

    let collisions = model.title_collisions();
    if !collisions.is_empty() {
        writeln!(
            out,
            "error: {name} has siblings that share a title, and filenames encode ancestry, so \
             importing them would put two pages on names nobody chose. Rename them in \
             CherryTree and import again. Nothing has been changed."
        )?;
        for t in &collisions {
            writeln!(out, "  colliding title: {t}")?;
        }
        return Ok(2);
    }

    if plans.contains(&Plan::Occupied) {
        writeln!(
            out,
            "error: some pages this import would write already exist and were not made by \
             importing this notebook. They are somebody's work, so nothing has been changed. \
             Remove or rename them, or import into an empty wiki."
        )?;
        write_plan(&model, &plans, out)?;
        return Ok(2);
    }

    // A parent must exist before its child, and the model stores parents as
    // indices. Checking rather than assuming: a reordering upstream would
    // otherwise show up as a confusing store error deep in the write loop.
    for (i, n) in model.nodes.iter().enumerate() {
        if let Some(p) = n.parent {
            if p >= i {
                writeln!(
                    out,
                    "error: internal: node {i} lists parent {p}, which is not earlier in the \
                     file. Nothing has been changed."
                )?;
                return Ok(2);
            }
        }
    }

    let git = match berrywiki_git::GitRepo::open(wiki) {
        Ok(g) => g,
        Err(e) => {
            writeln!(
                out,
                "error: {e}; an import writes many pages at once, so it is committed as one \
                 change you can undo with one command. Run `git init` in the wiki folder first. \
                 Nothing has been changed."
            )?;
            return Ok(2);
        }
    };
    let mut sync = berrywiki_sync::SyncedStore::new(store, git);

    // ---- the write ----
    //
    // Through `store_mut`, which does not commit, so the whole import lands in
    // the single commit below rather than one commit per page.
    let mut created = 0usize;
    let mut attached = 0usize;
    for (n, p) in model.nodes.iter().zip(&plans) {
        if *p != Plan::Create {
            continue;
        }
        let parent_id = n.parent.map(|i| model.nodes[i].id.clone());
        let input = CreatePageInput {
            id: n.id.clone(),
            title: n.title.clone(),
            parent_id,
            position: n.position,
            kind: PageKind::Page,
            tags: n.tags.clone(),
            body: n.body.clone(),
            // What makes a second import a no-op instead of a duplicate.
            extra: vec![format!("source: {}", n.marker)],
        };
        if let Err(e) = sync.store_mut().create_page(input) {
            writeln!(
                out,
                "error: writing page {:?} failed: {e}. {created} page(s) were already written \
                 and are NOT committed; `git status` shows them and `git checkout -- .` \
                 discards them.",
                n.title
            )?;
            return Ok(2);
        }
        created += 1;

        for asset in &n.assets {
            match sync
                .store_mut()
                .add_attachment(&n.id, &asset.filename, &asset.bytes)
            {
                Ok(_) => attached += 1,
                Err(e) => {
                    writeln!(
                        out,
                        "warning: attachment {:?} on page {:?} was not written: {e}",
                        asset.filename, n.title
                    )?;
                }
            }
        }
    }

    // Belt and braces: `create_page` keeps the sidebar current, but the import
    // must not depend on that to be true.
    if let Err(e) = sync.store_mut().regenerate_sidebar() {
        writeln!(out, "error: regenerating the sidebar failed: {e}")?;
        return Ok(2);
    }

    let message = format!(
        "Import {} pages from {name}\n\nSource SHA-256: {}\n",
        created, model.source_hash
    );
    match sync.commit(&message) {
        Ok(Some(_)) => {}
        Ok(None) => {
            writeln!(out, "note: nothing changed, so no commit was made.")?;
        }
        Err(e) => {
            writeln!(
                out,
                "error: the pages were written but committing them failed: {e}. They are in the \
                 working tree; `git status` shows them."
            )?;
            return Ok(2);
        }
    }

    // The notebook is read-only input, and saying so is cheap. Re-hashing the
    // file we were handed turns "we did not touch your notes" from a promise
    // into a measurement.
    match std::fs::read(source) {
        Ok(after) if berrywiki_import::sha256_hex(&after) == model.source_hash => {}
        Ok(_) => {
            writeln!(
                out,
                "warning: {name} changed while it was being imported. The wiki holds what the \
                 file said when the import started."
            )?;
        }
        Err(e) => {
            writeln!(
                out,
                "warning: {name} could not be re-read to confirm it was left alone: {e}"
            )?;
        }
    }

    let text = if as_json {
        report::json(&model, &run)
    } else {
        report::asciidoc(&model, &run)
    };
    write!(out, "{text}")?;
    writeln!(
        out,
        "\nWrote {created} page(s) and {attached} attachment(s) in one commit. Run `berrywiki \
         serve {}` to read them, and `git push` when you want them on GitHub.",
        wiki.display()
    )?;
    Ok(0)
}
