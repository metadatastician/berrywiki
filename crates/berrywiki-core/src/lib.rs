//! # berrywiki-core
//!
//! The deterministic, storage-agnostic heart of BerryWiki.
//!
//! This crate turns raw Markdown page sources into a navigable notebook: it
//! parses the hidden BerryWiki metadata block, extracts headings and links,
//! builds the page hierarchy from stable ids, computes backlinks, reports
//! consistency diagnostics, and generates a deterministic GitHub `_Sidebar.md`.
//!
//! It performs **no** I/O and depends on **no** browser, git or GitHub APIs, so
//! it can be exhaustively unit-tested and reused behind any `WikiStore`
//! adapter (see the forthcoming `berrywiki-store` crate).
//!
//! ## Pipeline
//!
//! ```text
//! raw source --parse--> WikiPage --build--> PageGraph --generate--> _Sidebar.md
//! ```
//!
//! ```
//! use berrywiki_core::{WikiPage, PageGraph, generate_sidebar, SidebarOptions};
//!
//! let home = WikiPage::parse(
//!     "Home.md",
//!     "<!-- berrywiki\nid: home\nparent: null\nposition: 0\nkind: page\ntags: []\narchived: false\n-->\n\n# Home\n\nSee [[Guide]].\n",
//! );
//! let guide = WikiPage::parse(
//!     "Guide.md",
//!     "<!-- berrywiki\nid: guide\nparent: home\nposition: 0\nkind: page\ntags: []\narchived: false\n-->\n\n# Guide\n",
//! );
//! let graph = PageGraph::build(vec![home, guide]);
//! assert_eq!(graph.backlinks_of("guide").len(), 1);
//! let sidebar = generate_sidebar(&graph, &SidebarOptions::default());
//! assert!(sidebar.contains("[Home](Home)"));
//! ```

pub mod diagnostics;
pub mod graph;
pub mod metadata;
pub mod page;
pub mod sidebar;

pub use diagnostics::{Diagnostic, Severity};
pub use graph::{Backlink, PageGraph};
pub use metadata::{
    parse_source, sanitises_field, serialize_metadata, serialize_source, PageKind, PageMetadata,
    ParsedSource,
};
pub use page::{extract_headings, extract_links, slug, PageHeading, PageLink, WikiPage};
pub use sidebar::{generate_sidebar, SidebarOptions};
