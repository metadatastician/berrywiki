// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! The neutral import model: what any source notebook is reduced to before
//! anything is written.
//!
//! Nothing here mentions CherryTree. That is the point: the Zim importer
//! (P5-import-zim) produces the same model, so the apply side, the reports
//! and their tests are written once. The model is also deliberately inert —
//! it holds no paths, no handles and no store — so a dry run and a real
//! import build exactly the same value and only the caller differs.

use berrywiki_core::Diagnostic;

/// A file to be written beside a page, e.g. an image lifted out of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportAsset {
    /// Filename within the page's asset folder. Always store-legal.
    pub filename: String,
    pub bytes: Vec<u8>,
}

/// One page-to-be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportNode {
    /// BerryWiki page id, derived from the source identity so that importing
    /// the same notebook twice mints the same ids (see [`crate::marker`]).
    pub id: String,
    /// The node's identity in the source notebook, e.g. a CherryTree
    /// `unique_id`. Used to resolve internal links and to build the marker.
    pub source_id: String,
    /// Index into [`ImportModel::nodes`] of this node's parent.
    pub parent: Option<usize>,
    pub position: i64,
    pub title: String,
    pub tags: Vec<String>,
    /// Markdown body, with internal links already resolved.
    pub body: String,
    pub assets: Vec<ImportAsset>,
    /// Value of the `source:` metadata key that makes a re-import a no-op.
    pub marker: String,
}

/// Everything one source notebook becomes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportModel {
    /// Pre-order, so a parent always precedes its children and positions are
    /// sibling-relative.
    pub nodes: Vec<ImportNode>,
    pub diagnostics: Vec<Diagnostic>,
    /// SHA-256 of the source file, lower-case hex.
    pub source_hash: String,
}

impl ImportModel {
    /// Deepest nesting level, counting a root node as depth 1.
    pub fn depth(&self) -> usize {
        let mut best = 0;
        for (i, _) in self.nodes.iter().enumerate() {
            let mut d = 1;
            let mut cur = self.nodes[i].parent;
            while let Some(p) = cur {
                d += 1;
                cur = self.nodes[p].parent;
            }
            best = best.max(d);
        }
        best
    }

    /// Titles that collide among siblings. ADR-0001 builds a filename from
    /// the ancestor titles, so two siblings sharing a title would collide on
    /// disk; the dry run reports these before anything is written.
    pub fn title_collisions(&self) -> Vec<String> {
        let mut collisions = Vec::new();
        let mut seen: Vec<(Option<usize>, String)> = Vec::new();
        for n in &self.nodes {
            let key = (n.parent, n.title.clone());
            if seen.contains(&key) {
                if !collisions.contains(&n.title) {
                    collisions.push(n.title.clone());
                }
            } else {
                seen.push(key);
            }
        }
        collisions
    }

    /// Every distinct diagnostic code, with how many times it fired. This is
    /// the lossiness summary a reader actually acts on.
    pub fn loss_summary(&self) -> Vec<(String, usize)> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for d in &self.diagnostics {
            match counts.iter_mut().find(|(c, _)| *c == d.code) {
                Some((_, n)) => *n += 1,
                None => counts.push((d.code.clone(), 1)),
            }
        }
        counts.sort_by(|a, b| a.0.cmp(&b.0));
        counts
    }

    pub fn asset_count(&self) -> usize {
        self.nodes.iter().map(|n| n.assets.len()).sum()
    }
}
