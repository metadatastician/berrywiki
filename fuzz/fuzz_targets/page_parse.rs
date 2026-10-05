// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Page parsing (headings, links, slugs, titles) never panics on any input.
#![no_main]

use berrywiki_core::page::{derive_title, extract_headings, extract_links, slug, WikiPage};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|src: &str| {
    let page = WikiPage::parse("Fuzz--Page.md", src);
    let _ = page.wiki_link_target();
    let _ = page.parent_id();
    let headings = extract_headings(src);
    let _ = derive_title(&headings, "Fuzz--Page.md");
    let _ = extract_links(src);
    let s = slug(src);
    assert!(
        !s.contains(char::is_whitespace),
        "slug kept whitespace: {s:?}"
    );
});
