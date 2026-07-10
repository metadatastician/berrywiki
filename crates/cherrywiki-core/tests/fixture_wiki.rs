//! Integration test: load the real fixture wiki from disk and exercise the
//! whole pipeline (parse -> graph -> backlinks -> sidebar -> diagnostics).
//!
//! This is the closest the core crate gets to I/O: it only *reads* fixture
//! files; the engine itself stays I/O-free.

use std::fs;
use std::path::PathBuf;

use cherrywiki_core::{generate_sidebar, PageGraph, SidebarOptions, WikiPage};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-wiki")
        .canonicalize()
        .expect("fixture dir exists")
}

fn load_pages() -> Vec<WikiPage> {
    let dir = fixture_dir();
    let mut pages = Vec::new();
    for entry in fs::read_dir(&dir).expect("read fixture dir") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".md") || name == "_Sidebar.md" {
            continue;
        }
        let source = fs::read_to_string(&path).expect("read fixture page");
        pages.push(WikiPage::parse(name, source));
    }
    pages
}

#[test]
fn fixture_loads_all_pages() {
    let pages = load_pages();
    assert_eq!(pages.len(), 10, "expected 10 content pages in the fixture");
}

#[test]
fn fixture_builds_hierarchy() {
    let graph = PageGraph::build(load_pages());
    let home_id = "0195f6d0-0000-7000-8000-000000000001";
    assert!(graph.get(home_id).is_some(), "Home page addressable by id");
    let children = graph.children_of(home_id);
    assert!(
        children.iter().any(|c| c.title == "Teaching"),
        "Teaching is a child of Home"
    );
    // The legacy page (no metadata) and Home are both roots.
    let root_titles: Vec<&str> = graph.roots().iter().map(|p| p.title.as_str()).collect();
    assert!(root_titles.contains(&"Home"));
    assert!(root_titles.contains(&"Plain Legacy Page"));
}

#[test]
fn fixture_backlinks_resolve() {
    let graph = PageGraph::build(load_pages());
    let home_id = "0195f6d0-0000-7000-8000-000000000001";
    // Sandbox and Plain Legacy Page both link to Home.
    let bl = graph.backlinks_of(home_id);
    assert!(bl.len() >= 2, "Home has at least two backlinks, got {}", bl.len());
}

#[test]
fn fixture_reports_expected_diagnostics() {
    let graph = PageGraph::build(load_pages());
    let codes: Vec<&str> = graph.diagnostics().iter().map(|d| d.code.as_str()).collect();
    assert!(codes.contains(&"link.broken"), "broken [[Nonexistent Page]] link");
    assert!(codes.contains(&"metadata.bad-position"), "malformed position");
    assert!(codes.contains(&"metadata.bad-archived"), "malformed archived");
}

#[test]
fn fixture_sidebar_is_deterministic_and_excludes_archived() {
    let graph = PageGraph::build(load_pages());
    let a = generate_sidebar(&graph, &SidebarOptions::default());
    let b = generate_sidebar(&graph, &SidebarOptions::default());
    assert_eq!(a, b, "sidebar generation is deterministic");
    assert!(a.contains("[Home](Home)"));
    assert!(!a.contains("Archived Old Page"), "archived page excluded by default");
}

#[test]
fn fixture_metadata_round_trips() {
    // The Assessment Plan page: parse -> serialise -> parse yields equal metadata.
    let dir = fixture_dir();
    let src = fs::read_to_string(dir.join("Teaching--Course-A--Assessment-Plan.md")).unwrap();
    let parsed = cherrywiki_core::parse_source(&src);
    let meta = parsed.metadata.expect("has metadata");
    let reserialised = cherrywiki_core::serialize_source(Some(&meta), &parsed.body);
    let reparsed = cherrywiki_core::parse_source(&reserialised);
    assert_eq!(reparsed.metadata.unwrap(), meta);
}
