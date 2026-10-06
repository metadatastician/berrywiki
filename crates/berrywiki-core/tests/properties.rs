// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Property tests for INV-1, INV-2 and INV-3 (v1 criterion T1).
//!
//! The example tests in `src/` pin known cases; these check the invariants
//! over thousands of generated ones. The generator is hand-rolled and seeded
//! (no third-party crate, per ADR-0015) and every failure names its case
//! seed, so `PROPERTY_SEED=<n>` replays exactly that case.

use berrywiki_core::metadata::{parse_source, serialize_source};
use berrywiki_core::{generate_sidebar, PageGraph, SidebarOptions, WikiPage};
use std::collections::HashSet;

/// Cases per property. Override with `PROPERTY_CASES` for a longer campaign.
fn cases() -> u64 {
    std::env::var("PROPERTY_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000)
}

/// The case seeds to run: one fixed seed when `PROPERTY_SEED` is set (replay),
/// otherwise `0..cases()`.
fn seeds() -> Vec<u64> {
    match std::env::var("PROPERTY_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(s) => vec![s],
        None => (0..cases()).collect(),
    }
}

/// A small deterministic generator (SplitMix64).
struct Gen(u64);

impl Gen {
    /// A generator for one case.
    fn new(seed: u64) -> Self {
        Gen(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    /// The next 64 random bits.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n` (n > 0).
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// One element of `items`.
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }

    /// A short string from an alphabet chosen to hit the parser's edges:
    /// separators, markers, control characters, CR, and non-ASCII.
    fn awkward(&mut self, max: usize) -> String {
        const PIECES: &[&str] = &[
            "a",
            "Z",
            "7",
            " ",
            "-",
            ":",
            "-->",
            "<!--",
            "null",
            "\r",
            "\u{0}",
            "\u{1}",
            "\t",
            "é",
            "語",
            "[",
            "]",
            ",",
            "#",
            "berrywiki",
            "- ",
            "  ",
        ];
        (0..self.below(max + 1))
            .map(|_| self.pick(PIECES))
            .collect()
    }

    /// A line ending, usually `\n`, sometimes CRLF or an odd mixture.
    fn eol(&mut self) -> &'static str {
        ["\n", "\n", "\n", "\r\n", "\r\r\n"][self.below(5)]
    }
}

/// A page source that is roughly well-formed but full of awkward values.
fn page_source(g: &mut Gen) -> String {
    const KEYS: &[&str] = &[
        "id",
        "parent",
        "position",
        "kind",
        "tags",
        "archived",
        "icon",
        "x-->y",
        "weird key",
    ];
    let mut s = String::new();
    for _ in 0..g.below(2) {
        s.push_str(g.eol()); // leading blank lines are allowed
    }
    s.push_str("<!-- berrywiki");
    s.push_str(g.eol());
    for _ in 0..g.below(8) {
        let key = g.pick(KEYS);
        let value = g.awkward(6);
        s.push_str(&format!("{key}: {value}"));
        s.push_str(g.eol());
        if g.below(4) == 0 {
            s.push_str(&format!("  - {}", g.awkward(4)));
            s.push_str(g.eol());
        }
    }
    if g.below(10) != 0 {
        s.push_str("-->");
        s.push_str(g.eol());
    }
    for _ in 0..g.below(4) {
        s.push_str(&g.awkward(10));
        s.push_str(g.eol());
    }
    if g.below(3) == 0 {
        s.push_str(&g.awkward(5)); // no final newline
    }
    s
}

#[test]
fn inv1_serialisation_reaches_a_fixed_point() {
    for seed in seeds() {
        let src = page_source(&mut Gen::new(seed));
        let first = parse_source(&src);
        let once = serialize_source(first.metadata.as_ref(), &first.body);
        let second = parse_source(&once);
        let twice = serialize_source(second.metadata.as_ref(), &second.body);
        assert_eq!(once, twice, "INV-1 seed {seed}: not idempotent for {src:?}");
        assert_eq!(
            first.metadata, second.metadata,
            "INV-1 seed {seed}: metadata moved"
        );
        if first.metadata.is_some() {
            // The body is the file's own bytes, carried through unchanged.
            assert_eq!(first.body, second.body, "INV-1 seed {seed}: body changed");
        } else {
            assert_eq!(
                first.body, src,
                "INV-1 seed {seed}: block-less page altered"
            );
        }
    }
}

/// A page with the given id, parent, position and archived flag.
fn page(id: &str, parent: Option<&str>, position: i64, archived: bool) -> WikiPage {
    let parent = parent.unwrap_or("null");
    WikiPage::parse(
        format!("{id}.md"),
        format!(
            "<!-- berrywiki\nid: {id}\nparent: {parent}\nposition: {position}\nkind: page\n\
tags: []\narchived: {archived}\n-->\n\n# Title {id}\n"
        ),
    )
}

/// A random page set: ids `p0..pn`, each parent random (none, any page
/// including itself, or a missing id), positions colliding on purpose.
fn page_set(g: &mut Gen) -> Vec<WikiPage> {
    let n = 1 + g.below(12);
    (0..n)
        .map(|i| {
            let parent = match g.below(5) {
                0 => None,
                1 => Some("missing".to_string()),
                _ => Some(format!("p{}", g.below(n))),
            };
            page(
                &format!("p{i}"),
                parent.as_deref(),
                (g.below(3) as i64) * 10,
                g.below(6) == 0,
            )
        })
        .collect()
}

/// `pages` in a random order.
fn shuffled(g: &mut Gen, mut pages: Vec<WikiPage>) -> Vec<WikiPage> {
    for i in (1..pages.len()).rev() {
        pages.swap(i, g.below(i + 1));
    }
    pages
}

#[test]
fn inv2_the_parent_relation_is_always_a_forest() {
    for seed in seeds() {
        let mut g = Gen::new(seed);
        let pages = page_set(&mut g);
        let ids: HashSet<String> = pages.iter().map(|p| p.id.clone()).collect();
        let graph = PageGraph::build(pages.clone());

        // Every page appears exactly once in the walk: no cycle can repeat a
        // page or lose one, whatever the parent pointers say.
        let walk = graph.walk();
        let walked: Vec<&str> = walk.iter().map(|(_, p)| p.id.as_str()).collect();
        let unique: HashSet<&str> = walked.iter().copied().collect();
        assert_eq!(walked.len(), ids.len(), "INV-2 seed {seed}: walk length");
        assert_eq!(
            unique.len(),
            ids.len(),
            "INV-2 seed {seed}: a page repeated"
        );

        // No page is its own ancestor: following parents from any page in the
        // built tree reaches a root without revisiting.
        for (_, p) in &walk {
            let mut seen = HashSet::new();
            let mut cur = p.id.clone();
            while let Some(parent) = graph
                .pages()
                .iter()
                .find(|q| graph.children_of(&q.id).iter().any(|c| c.id == cur))
                .map(|q| q.id.clone())
            {
                assert!(
                    seen.insert(parent.clone()),
                    "INV-2 seed {seed}: cycle at {cur}"
                );
                cur = parent;
            }
        }

        // Deterministic: any input order walks the same tree in the same order.
        let again = PageGraph::build(shuffled(&mut g, pages));
        let walked_again: Vec<(usize, &str)> = again
            .walk()
            .iter()
            .map(|(d, p)| (*d, p.id.as_str()))
            .collect();
        let first: Vec<(usize, &str)> = walk.iter().map(|(d, p)| (*d, p.id.as_str())).collect();
        assert_eq!(
            first, walked_again,
            "INV-2 seed {seed}: order depends on input order"
        );
    }
}

#[test]
fn inv3_the_sidebar_is_a_pure_function_of_the_page_set() {
    let options = SidebarOptions::default();
    for seed in seeds() {
        let mut g = Gen::new(seed);
        let pages = page_set(&mut g);
        let archived: Vec<String> = pages
            .iter()
            .filter(|p| p.is_archived())
            .map(|p| format!("Title {}", p.id))
            .collect();
        let a = generate_sidebar(&PageGraph::build(pages.clone()), &options);
        let b = generate_sidebar(&PageGraph::build(shuffled(&mut g, pages)), &options);
        assert_eq!(a, b, "INV-3 seed {seed}: sidebar depends on input order");
        for t in &archived {
            // `Title p1` must not match `Title p10`: compare whole link labels.
            assert!(
                !a.contains(&format!("[{t}]")),
                "INV-3 seed {seed}: archived {t} listed"
            );
        }
    }
}
