//! Deterministic generation of GitHub Wiki's `_Sidebar.md`.
//!
//! Requirements honoured:
//! * Deterministic output (same graph → byte-identical string).
//! * Archived pages excluded unless configured; excluding a page prunes its
//!   whole subtree so no orphan links appear.
//! * Respects a maximum depth.
//! * GitHub-compatible links (`[Title](Page-Slug)`), using each page's flat
//!   filename stem as the wiki target.
//!
//! The *decision* of whether to write the file (skip when unchanged, commit in
//! the same transaction as the tree change) belongs to the store, not here.

use crate::graph::PageGraph;
use crate::page::WikiPage;

/// Options controlling sidebar rendering (mirrors `.berrywiki.json` `sidebar`).
#[derive(Debug, Clone)]
pub struct SidebarOptions {
    pub include_archived: bool,
    /// Maximum 1-based depth to render (roots are depth 1).
    pub maximum_depth: usize,
    /// Heading shown at the top of the sidebar.
    pub heading: String,
}

impl Default for SidebarOptions {
    fn default() -> Self {
        SidebarOptions {
            include_archived: false,
            maximum_depth: 5,
            heading: "Notebook".to_string(),
        }
    }
}

/// Render `_Sidebar.md` for the given graph.
pub fn generate_sidebar(graph: &PageGraph, options: &SidebarOptions) -> String {
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(&options.heading);
    out.push_str("\n\n");
    for root in graph.roots() {
        render(graph, root, 1, options, &mut out);
    }
    out
}

fn render(
    graph: &PageGraph,
    page: &WikiPage,
    depth: usize,
    options: &SidebarOptions,
    out: &mut String,
) {
    if depth > options.maximum_depth {
        return;
    }
    if page.is_archived() && !options.include_archived {
        return; // prune this page and its subtree
    }
    let indent = "  ".repeat(depth - 1);
    out.push_str(&format!(
        "{indent}- [{}]({})\n",
        escape_label(&page.title),
        page.wiki_link_target()
    ));
    for child in graph.children_of(&page.id) {
        render(graph, child, depth + 1, options, out);
    }
}

/// Escape the characters that would break a Markdown link label.
fn escape_label(title: &str) -> String {
    title.replace('[', "\\[").replace(']', "\\]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::WikiPage;

    fn page(id: &str, parent: Option<&str>, pos: i64, path: &str, title: &str, archived: bool) -> WikiPage {
        let parent_line = match parent {
            Some(p) => format!("parent: {p}\n"),
            None => "parent: null\n".to_string(),
        };
        let src = format!(
            "<!-- berrywiki\nid: {id}\n{parent_line}position: {pos}\nkind: page\ntags: []\narchived: {archived}\n-->\n\n# {title}\n"
        );
        WikiPage::parse(path, src)
    }

    fn sample_graph() -> PageGraph {
        PageGraph::build(vec![
            page("home", None, 0, "Home.md", "Home", false),
            page("teach", None, 10, "Teaching.md", "Teaching", false),
            page("courseA", Some("teach"), 0, "Teaching--Course-A.md", "Course A", false),
            page("plan", Some("courseA"), 0, "Teaching--Course-A--Assessment-Plan.md", "Assessment Plan", false),
            page("old", None, 20, "Old.md", "Old Page", true),
        ])
    }

    #[test]
    fn generates_expected_nested_sidebar() {
        let g = sample_graph();
        let s = generate_sidebar(&g, &SidebarOptions::default());
        // Note: built with concat! (not `\`-continuation, which strips the
        // leading indentation we are specifically asserting on).
        let expected = concat!(
            "# Notebook\n",
            "\n",
            "- [Home](Home)\n",
            "- [Teaching](Teaching)\n",
            "  - [Course A](Teaching--Course-A)\n",
            "    - [Assessment Plan](Teaching--Course-A--Assessment-Plan)\n",
        );
        assert_eq!(s, expected);
    }

    #[test]
    fn excludes_archived_by_default() {
        let g = sample_graph();
        let s = generate_sidebar(&g, &SidebarOptions::default());
        assert!(!s.contains("Old Page"));
    }

    #[test]
    fn includes_archived_when_configured() {
        let g = sample_graph();
        let opts = SidebarOptions {
            include_archived: true,
            ..SidebarOptions::default()
        };
        let s = generate_sidebar(&g, &opts);
        assert!(s.contains("[Old Page](Old)"));
    }

    #[test]
    fn respects_maximum_depth() {
        let g = sample_graph();
        let opts = SidebarOptions {
            maximum_depth: 2,
            ..SidebarOptions::default()
        };
        let s = generate_sidebar(&g, &opts);
        assert!(s.contains("Course A")); // depth 2
        assert!(!s.contains("Assessment Plan")); // depth 3, pruned
    }

    #[test]
    fn output_is_deterministic() {
        let a = generate_sidebar(&sample_graph(), &SidebarOptions::default());
        let b = generate_sidebar(&sample_graph(), &SidebarOptions::default());
        assert_eq!(a, b);
    }
}
