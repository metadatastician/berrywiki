//! In-memory page model plus pure extractors (headings, links, title, slug).
//!
//! Everything here is derived deterministically from a page's raw source; there
//! is no I/O. A [`WikiPage`] is what a `WikiStore` adapter hands to the engine.

use crate::diagnostics::Diagnostic;
use crate::metadata::{parse_source, PageMetadata};

/// A heading extracted from the body (ATX `#` form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHeading {
    pub depth: u8,
    pub text: String,
    /// GitHub-compatible anchor slug (before duplicate de-collision).
    pub anchor: String,
}

/// An outgoing link, whether authored as a `[[wiki link]]` or a standard
/// `[label](Target)` Markdown link to another wiki page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLink {
    /// The exact source text of the link, for diagnostics and rewriting.
    pub raw: String,
    pub label: Option<String>,
    /// The link target as written (page title or slug), heading stripped.
    pub target_text: String,
    pub target_heading: Option<String>,
    /// Resolved page id, filled in later by the graph. `None` until resolved.
    pub target_page_id: Option<String>,
    pub resolved: bool,
}

/// A fully-parsed wiki page.
#[derive(Debug, Clone)]
pub struct WikiPage {
    /// Stable id from metadata, or a fallback derived from `path` when the page
    /// is not yet managed.
    pub id: String,
    pub title: String,
    /// Path relative to the wiki root, e.g. `Teaching--Course-A--Plan.md`.
    pub path: String,
    pub source: String,
    pub body: String,
    pub metadata: Option<PageMetadata>,
    pub headings: Vec<PageHeading>,
    pub outgoing_links: Vec<PageLink>,
    pub diagnostics: Vec<Diagnostic>,
}

impl WikiPage {
    /// Parse a page from its raw source and repository-relative path.
    pub fn parse(path: impl Into<String>, source: impl Into<String>) -> Self {
        let path = path.into();
        let source = source.into();
        let parsed = parse_source(&source);
        let mut diagnostics = parsed.diagnostics;

        let headings = extract_headings(&parsed.body);
        let outgoing_links = extract_links(&parsed.body);

        let title = derive_title(&headings, &path);

        // A managed page uses its metadata id; an unmanaged page falls back to
        // its path so it can still appear in the tree (as a root).
        let id = match &parsed.metadata {
            Some(m) if !m.id.is_empty() => m.id.clone(),
            _ => path.clone(),
        };

        for d in &mut diagnostics {
            if d.page.is_none() {
                d.page = Some(id.clone());
            }
        }

        WikiPage {
            id,
            title,
            path,
            source,
            body: parsed.body,
            metadata: parsed.metadata,
            headings,
            outgoing_links,
            diagnostics,
        }
    }

    /// GitHub wiki link target for this page: the filename stem (no `.md`).
    /// Used by sidebar generation and link rewriting.
    pub fn wiki_link_target(&self) -> &str {
        self.path
            .strip_suffix(".md")
            .unwrap_or(&self.path)
    }

    pub fn is_archived(&self) -> bool {
        self.metadata.as_ref().map(|m| m.archived).unwrap_or(false)
    }

    pub fn parent_id(&self) -> Option<&str> {
        self.metadata.as_ref().and_then(|m| m.parent_id.as_deref())
    }

    pub fn position(&self) -> i64 {
        self.metadata.as_ref().map(|m| m.position).unwrap_or(0)
    }
}

/// Extract ATX headings, ignoring `#` inside fenced code blocks.
pub fn extract_headings(body: &str) -> Vec<PageHeading> {
    let mut headings = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            let mut depth = 1u8;
            let mut rest = rest;
            while let Some(r) = rest.strip_prefix('#') {
                depth += 1;
                rest = r;
            }
            // Must be `#` followed by a space to be a heading.
            if let Some(text) = rest.strip_prefix(' ') {
                let text = text.trim().trim_end_matches('#').trim().to_string();
                let anchor = slug(&text);
                headings.push(PageHeading { depth, text, anchor });
            }
        }
    }
    headings
}

/// GitHub-compatible heading anchor slug (single-heading; duplicate
/// de-collision like `-1`/`-2` is the graph's concern, not this function's).
pub fn slug(text: &str) -> String {
    let mut s = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            for lc in c.to_lowercase() {
                s.push(lc);
            }
        } else if c == ' ' {
            s.push('-');
        }
        // all other characters are dropped
    }
    s
}

/// Extract outgoing links: both `[[wiki links]]` and internal `[label](Target)`
/// Markdown links. External links (with a URL scheme or `//`) are ignored.
pub fn extract_links(body: &str) -> Vec<PageLink> {
    let mut links = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        extract_wiki_links(line, &mut links);
        extract_markdown_links(line, &mut links);
    }
    links
}

fn extract_wiki_links(line: &str, out: &mut Vec<PageLink>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(end) = line[i + 2..].find("]]") {
                let inner = &line[i + 2..i + 2 + end];
                let raw = format!("[[{inner}]]");
                out.push(parse_wiki_inner(inner, raw));
                i = i + 2 + end + 2;
                continue;
            }
        }
        i += 1;
    }
}

fn parse_wiki_inner(inner: &str, raw: String) -> PageLink {
    // Forms: Target | Target#Heading | Target|Label | Target#Heading|Label
    let (target_part, label) = match inner.split_once('|') {
        Some((t, l)) => (t.trim(), Some(l.trim().to_string())),
        None => (inner.trim(), None),
    };
    let (target_text, target_heading) = match target_part.split_once('#') {
        Some((t, h)) => (t.trim().to_string(), Some(h.trim().to_string())),
        None => (target_part.to_string(), None),
    };
    PageLink {
        raw,
        label,
        target_text,
        target_heading,
        target_page_id: None,
        resolved: false,
    }
}

fn extract_markdown_links(line: &str, out: &mut Vec<PageLink>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip wiki links so we don't double-count their inner `[...]`.
        if i + 1 < bytes.len() && bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(end) = line[i + 2..].find("]]") {
                i = i + 2 + end + 2;
                continue;
            }
        }
        if bytes[i] == b'[' {
            if let Some(close) = line[i + 1..].find(']') {
                let label_start = i + 1;
                let label_end = i + 1 + close;
                let after = label_end + 1;
                if after < bytes.len() && bytes[after] == b'(' {
                    if let Some(paren) = line[after + 1..].find(')') {
                        let target = &line[after + 1..after + 1 + paren];
                        let label = &line[label_start..label_end];
                        if let Some(link) = internal_md_link(label, target) {
                            out.push(link);
                        }
                        i = after + 1 + paren + 1;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
}

fn internal_md_link(label: &str, target: &str) -> Option<PageLink> {
    let target = target.trim();
    // Ignore external / absolute / anchor-only / image targets.
    if target.is_empty()
        || target.contains("://")
        || target.starts_with('/')
        || target.starts_with('#')
        || target.starts_with("mailto:")
    {
        return None;
    }
    let raw = format!("[{label}]({target})");
    let (path_part, heading) = match target.split_once('#') {
        Some((p, h)) => (p.trim(), Some(h.trim().to_string())),
        None => (target, None),
    };
    let target_text = path_part.strip_suffix(".md").unwrap_or(path_part).to_string();
    Some(PageLink {
        raw,
        label: Some(label.to_string()),
        target_text,
        target_heading: heading,
        target_page_id: None,
        resolved: false,
    })
}

/// Title = first H1 in the body, else the humanised filename stem.
pub fn derive_title(headings: &[PageHeading], path: &str) -> String {
    if let Some(h1) = headings.iter().find(|h| h.depth == 1) {
        return h1.text.clone();
    }
    let stem = path.strip_suffix(".md").unwrap_or(path);
    // Use the last hierarchy segment of a `--`-separated flat filename.
    let last = stem.rsplit("--").next().unwrap_or(stem);
    last.replace('-', " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_atx_headings_with_anchors() {
        let body = "# Title\n\n## Weighting Scheme\n\ntext\n### Sub-Point!\n";
        let hs = extract_headings(body);
        assert_eq!(hs.len(), 3);
        assert_eq!(hs[1].depth, 2);
        assert_eq!(hs[1].anchor, "weighting-scheme");
        assert_eq!(hs[2].anchor, "sub-point");
    }

    #[test]
    fn ignores_headings_in_code_fences() {
        let body = "# Real\n\n```\n# not a heading\n```\n## Also Real\n";
        let hs = extract_headings(body);
        assert_eq!(hs.len(), 2);
        assert_eq!(hs[0].text, "Real");
        assert_eq!(hs[1].text, "Also Real");
    }

    #[test]
    fn parses_wiki_link_variants() {
        let body = "See [[Assessment Plan]] and [[Assessment Plan#Weighting]] \
                    and [[Assessment Plan|assessment details]].";
        let ls = extract_links(body);
        assert_eq!(ls.len(), 3);
        assert_eq!(ls[0].target_text, "Assessment Plan");
        assert_eq!(ls[1].target_heading.as_deref(), Some("Weighting"));
        assert_eq!(ls[2].label.as_deref(), Some("assessment details"));
    }

    #[test]
    fn parses_internal_markdown_links_only() {
        let body =
            "[a](Other-Page) [ext](https://example.com) [root](/x) [img](assets/p.png#f)";
        let ls = extract_links(body);
        // Other-Page and assets/p.png are internal; the http and /x links are not.
        let targets: Vec<&str> = ls.iter().map(|l| l.target_text.as_str()).collect();
        assert!(targets.contains(&"Other-Page"));
        assert!(targets.contains(&"assets/p.png"));
        assert!(!targets.iter().any(|t| t.contains("example.com")));
    }

    #[test]
    fn title_from_h1_then_filename() {
        let p = WikiPage::parse("Teaching--Course-A--Plan.md", "no heading here\n");
        assert_eq!(p.title, "Plan");
        let p2 = WikiPage::parse("X.md", "# Real Title\n");
        assert_eq!(p2.title, "Real Title");
    }

    #[test]
    fn unmanaged_page_gets_path_id() {
        let p = WikiPage::parse("Home.md", "# Home\n");
        assert_eq!(p.id, "Home.md");
        assert!(p.metadata.is_none());
    }

    #[test]
    fn wiki_link_target_strips_md() {
        let p = WikiPage::parse("A--B.md", "# t\n");
        assert_eq!(p.wiki_link_target(), "A--B");
    }
}
