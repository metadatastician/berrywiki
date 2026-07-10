//! The derived page graph: hierarchy, sibling ordering, backlinks and
//! consistency diagnostics.
//!
//! The graph is *entirely* rebuildable from a set of [`WikiPage`]s (Non-
//! negotiable requirement 3 & 15). Parent/child relationships come from stable
//! metadata ids, never from filenames.

use std::collections::HashMap;

use crate::diagnostics::Diagnostic;
use crate::page::{PageLink, WikiPage};

/// One incoming link into a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backlink {
    pub from_id: String,
    pub from_title: String,
    pub link: PageLink,
}

/// The whole derived graph.
pub struct PageGraph {
    pages: Vec<WikiPage>,
    id_to_index: HashMap<String, usize>,
    children: HashMap<String, Vec<usize>>,
    roots: Vec<usize>,
    backlinks: HashMap<String, Vec<Backlink>>,
    diagnostics: Vec<Diagnostic>,
}

impl PageGraph {
    /// Build the graph from parsed pages. Order of the input does not affect the
    /// output (results are sorted deterministically).
    pub fn build(mut pages: Vec<WikiPage>) -> Self {
        let mut diagnostics: Vec<Diagnostic> = Vec::new();

        // Carry each page's own parse diagnostics into the graph.
        for p in &pages {
            diagnostics.extend(p.diagnostics.iter().cloned());
        }

        // --- id index + duplicate detection ---
        let mut id_to_index: HashMap<String, usize> = HashMap::new();
        for (idx, p) in pages.iter().enumerate() {
            if let Some(&first) = id_to_index.get(&p.id) {
                diagnostics.push(
                    Diagnostic::error(
                        "graph.duplicate-id",
                        format!(
                            "Duplicate page id {:?}: both {:?} and {:?}. Only the \
                             first is addressable; assign a unique id.",
                            p.id, pages[first].path, p.path
                        ),
                    )
                    .with_page(p.id.clone()),
                );
            } else {
                id_to_index.insert(p.id.clone(), idx);
            }
        }

        // --- link resolution + backlinks ---
        let mut by_target: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_title: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, p) in pages.iter().enumerate() {
            by_target
                .entry(p.wiki_link_target().to_lowercase())
                .or_default()
                .push(idx);
            by_title.entry(p.title.to_lowercase()).or_default().push(idx);
        }

        let mut backlinks: HashMap<String, Vec<Backlink>> = HashMap::new();
        // Resolve each page's outgoing links (mutating the stored link).
        for src_idx in 0..pages.len() {
            let from_id = pages[src_idx].id.clone();
            let from_title = pages[src_idx].title.clone();
            let links_len = pages[src_idx].outgoing_links.len();
            for l in 0..links_len {
                let target_text = pages[src_idx].outgoing_links[l].target_text.clone();
                let resolved = resolve_target(&target_text, &by_target, &by_title, &pages);
                match resolved {
                    Resolution::One(t_idx) => {
                        let target_id = pages[t_idx].id.clone();
                        let link = &mut pages[src_idx].outgoing_links[l];
                        link.target_page_id = Some(target_id.clone());
                        link.resolved = true;
                        backlinks.entry(target_id).or_default().push(Backlink {
                            from_id: from_id.clone(),
                            from_title: from_title.clone(),
                            link: pages[src_idx].outgoing_links[l].clone(),
                        });
                    }
                    Resolution::Ambiguous(indices) => {
                        let paths: Vec<&str> =
                            indices.iter().map(|&i| pages[i].path.as_str()).collect();
                        diagnostics.push(
                            Diagnostic::warning(
                                "link.ambiguous",
                                format!(
                                    "Link target {target_text:?} matches multiple pages ({paths:?}); \
                                     left unresolved. Disambiguate the title or link by path."
                                ),
                            )
                            .with_page(from_id.clone()),
                        );
                    }
                    Resolution::BrokenPage => diagnostics.push(
                        Diagnostic::warning(
                            "link.broken",
                            format!("Link target {target_text:?} does not match any page."),
                        )
                        .with_page(from_id.clone()),
                    ),
                    Resolution::NotAPage => { /* attachment / file link: ignore */ }
                }
            }
        }

        // --- hierarchy: parent pointers, cycle safety ---
        let mut parent_of: HashMap<String, String> = HashMap::new();
        for p in &pages {
            if let Some(parent) = p.parent_id() {
                if id_to_index.contains_key(parent) {
                    parent_of.insert(p.id.clone(), parent.to_string());
                }
            }
        }

        let mut children: HashMap<String, Vec<usize>> = HashMap::new();
        let mut roots: Vec<usize> = Vec::new();
        for (idx, p) in pages.iter().enumerate() {
            match p.parent_id() {
                None => roots.push(idx),
                Some(parent) => {
                    if !id_to_index.contains_key(parent) {
                        diagnostics.push(
                            Diagnostic::warning(
                                "graph.missing-parent",
                                format!(
                                    "Parent id {parent:?} not found; page treated as a root."
                                ),
                            )
                            .with_page(p.id.clone()),
                        );
                        roots.push(idx);
                    } else if ancestry_loops(&p.id, &parent_of) {
                        diagnostics.push(
                            Diagnostic::error(
                                "graph.cycle",
                                "Parent relationship forms a cycle; page treated as a root."
                                    .to_string(),
                            )
                            .with_page(p.id.clone()),
                        );
                        roots.push(idx);
                    } else {
                        children.entry(parent.to_string()).or_default().push(idx);
                    }
                }
            }
        }

        // --- deterministic ordering of siblings and roots ---
        let sort_key = |pages: &[WikiPage], idx: usize| {
            (
                pages[idx].position(),
                pages[idx].title.to_lowercase(),
                pages[idx].id.clone(),
            )
        };
        roots.sort_by(|&a, &b| sort_key(&pages, a).cmp(&sort_key(&pages, b)));
        for kids in children.values_mut() {
            kids.sort_by(|&a, &b| sort_key(&pages, a).cmp(&sort_key(&pages, b)));
        }

        PageGraph {
            pages,
            id_to_index,
            children,
            roots,
            backlinks,
            diagnostics,
        }
    }

    pub fn pages(&self) -> &[WikiPage] {
        &self.pages
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn get(&self, id: &str) -> Option<&WikiPage> {
        self.id_to_index.get(id).map(|&i| &self.pages[i])
    }

    /// Root page indices, deterministically ordered.
    pub fn roots(&self) -> Vec<&WikiPage> {
        self.roots.iter().map(|&i| &self.pages[i]).collect()
    }

    /// Direct children of a page id, deterministically ordered.
    pub fn children_of(&self, id: &str) -> Vec<&WikiPage> {
        self.children
            .get(id)
            .map(|v| v.iter().map(|&i| &self.pages[i]).collect())
            .unwrap_or_default()
    }

    /// Incoming links to a page id.
    pub fn backlinks_of(&self, id: &str) -> &[Backlink] {
        self.backlinks.get(id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Depth-first pre-order walk of the whole tree, yielding `(depth, page)`.
    pub fn walk(&self) -> Vec<(usize, &WikiPage)> {
        let mut out = Vec::new();
        for &r in &self.roots {
            self.walk_from(r, 0, &mut out);
        }
        out
    }

    fn walk_from<'a>(&'a self, idx: usize, depth: usize, out: &mut Vec<(usize, &'a WikiPage)>) {
        out.push((depth, &self.pages[idx]));
        let id = &self.pages[idx].id;
        if let Some(kids) = self.children.get(id) {
            for &k in kids {
                self.walk_from(k, depth + 1, out);
            }
        }
    }
}

enum Resolution {
    One(usize),
    Ambiguous(Vec<usize>),
    BrokenPage,
    NotAPage,
}

fn resolve_target(
    target: &str,
    by_target: &HashMap<String, Vec<usize>>,
    by_title: &HashMap<String, Vec<usize>>,
    _pages: &[WikiPage],
) -> Resolution {
    // Attachment / nested file references are not page links.
    if target.contains('/') || target.contains('.') {
        return Resolution::NotAPage;
    }
    let key = target.to_lowercase();
    let hits = by_target.get(&key).or_else(|| by_title.get(&key));
    match hits {
        Some(v) if v.len() == 1 => Resolution::One(v[0]),
        Some(v) if v.len() > 1 => {
            let mut sorted = v.clone();
            sorted.sort();
            Resolution::Ambiguous(sorted)
        }
        _ => Resolution::BrokenPage,
    }
}

/// True if following parent pointers from `start` revisits a node (a cycle in
/// the ancestry). Bounded by the number of entries, so it always terminates.
fn ancestry_loops(start: &str, parent_of: &HashMap<String, String>) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut cur = start.to_string();
    loop {
        if !seen.insert(cur.clone()) {
            return true;
        }
        match parent_of.get(&cur) {
            Some(p) => cur = p.clone(),
            None => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: &str, parent: Option<&str>, pos: i64, title: &str, body: &str) -> WikiPage {
        let parent_line = match parent {
            Some(p) => format!("parent: {p}\n"),
            None => "parent: null\n".to_string(),
        };
        let src = format!(
            "<!-- berrywiki\nid: {id}\n{parent_line}position: {pos}\nkind: page\ntags: []\narchived: false\n-->\n\n# {title}\n\n{body}\n"
        );
        WikiPage::parse(format!("{title}.md"), src)
    }

    #[test]
    fn builds_hierarchy_from_metadata() {
        let pages = vec![
            page("root", None, 0, "Home", "[[Child A]] and [[Child B]]"),
            page("b", Some("root"), 20, "Child B", "back to [[Home]]"),
            page("a", Some("root"), 10, "Child A", ""),
        ];
        let g = PageGraph::build(pages);
        let roots = g.roots();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].id, "root");
        let kids = g.children_of("root");
        // position 10 (a) before 20 (b), regardless of input order
        assert_eq!(kids.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn calculates_backlinks() {
        let pages = vec![
            page("root", None, 0, "Home", "see [[Child A]]"),
            page("a", Some("root"), 10, "Child A", "up to [[Home]]"),
        ];
        let g = PageGraph::build(pages);
        let bl_home = g.backlinks_of("root");
        assert_eq!(bl_home.len(), 1);
        assert_eq!(bl_home[0].from_id, "a");
        let bl_a = g.backlinks_of("a");
        assert_eq!(bl_a.len(), 1);
        assert_eq!(bl_a[0].from_id, "root");
    }

    #[test]
    fn detects_broken_link() {
        let pages = vec![page("root", None, 0, "Home", "see [[Nonexistent]]")];
        let g = PageGraph::build(pages);
        assert!(g.diagnostics().iter().any(|d| d.code == "link.broken"));
    }

    #[test]
    fn missing_parent_becomes_root_with_diagnostic() {
        let pages = vec![page("orphan", Some("ghost"), 0, "Orphan", "")];
        let g = PageGraph::build(pages);
        assert_eq!(g.roots().len(), 1);
        assert!(g.diagnostics().iter().any(|d| d.code == "graph.missing-parent"));
    }

    #[test]
    fn detects_cycle_without_infinite_loop() {
        let pages = vec![
            page("x", Some("y"), 0, "X", ""),
            page("y", Some("x"), 0, "Y", ""),
        ];
        let g = PageGraph::build(pages);
        assert!(g.diagnostics().iter().any(|d| d.code == "graph.cycle"));
        // Both salvaged as roots; no panic / hang.
        assert_eq!(g.roots().len(), 2);
    }

    #[test]
    fn detects_duplicate_id() {
        let pages = vec![
            page("dup", None, 0, "First", ""),
            page("dup", None, 1, "Second", ""),
        ];
        let g = PageGraph::build(pages);
        assert!(g.diagnostics().iter().any(|d| d.code == "graph.duplicate-id"));
    }

    #[test]
    fn walk_is_preorder_and_deterministic() {
        let pages = vec![
            page("a", Some("root"), 10, "A", ""),
            page("root", None, 0, "Home", ""),
            page("a1", Some("a"), 0, "A One", ""),
        ];
        let g = PageGraph::build(pages);
        let order: Vec<&str> = g.walk().iter().map(|(_, p)| p.id.as_str()).collect();
        assert_eq!(order, ["root", "a", "a1"]);
    }
}
