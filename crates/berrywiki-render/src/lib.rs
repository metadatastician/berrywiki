// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Markdown → HTML rendering for the BerryWiki SSR UI (ADR-0005).
//!
//! Wraps comrak with GitHub-Flavored-Markdown extensions and a deliberate,
//! stated safety policy. This produces a *preview*; the Markdown source is
//! always canonical.
//!
//! # Safety policy (the raw-HTML / no-`<script>` question, answered here)
//!
//! * **Raw HTML in the source is escaped, never emitted** (`render.unsafe_ =
//!   false`, plus the GFM `tagfilter`). No `<script>`, no event-handler
//!   attributes, and no injected markup from page content can ever reach the
//!   browser. This is enforced by comrak, not by a fragile substring scan.
//! * **Dangerous link/image URL schemes are neutralised at the AST level**
//!   before formatting: `javascript:`, `vbscript:` and non-image `data:` URLs
//!   are rewritten to `#`. Working on the parsed AST (not the output string)
//!   makes this robust against obfuscation that would defeat a regex.
//!
//! # Known, bounded divergence from GitHub
//!
//! GitHub's wiki renders a *sanitised subset* of inline HTML; BerryWiki
//! escapes inline HTML entirely. This is a safety-over-fidelity choice; it is
//! a divergence to record in the compatibility report, and it never affects
//! the canonical source. Exact comrak-vs-GitHub parity remains **unverified**
//! until the live spike.

use comrak::nodes::{AstNode, NodeValue};
use comrak::{format_html, parse_document, Arena, Options};

/// Render a Markdown fragment to a safe HTML fragment per the policy above.
pub fn render_markdown(markdown: &str) -> String {
    let options = safe_gfm_options();
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &options);
    neutralise_urls(root);
    let mut html = String::new();
    format_html(root, &options, &mut html).expect("formatting into a String cannot fail");
    html
}

/// The BerryWiki comrak configuration: GFM on, raw HTML off.
fn safe_gfm_options() -> Options<'static> {
    let mut o = Options::default();
    o.extension.table = true;
    o.extension.strikethrough = true;
    o.extension.tasklist = true;
    o.extension.autolink = true;
    o.extension.tagfilter = true;
    o.extension.footnotes = true;
    // SECURITY: escape raw HTML rather than pass it through.
    o.render.r#unsafe = false;
    o
}

/// Rewrite dangerous link/image URLs to `#`, walking the whole AST.
fn neutralise_urls<'a>(node: &'a AstNode<'a>) {
    {
        let mut data = node.data.borrow_mut();
        if let NodeValue::Link(ref mut link) | NodeValue::Image(ref mut link) = &mut data.value {
            if is_dangerous_url(&link.url) {
                link.url = "#".to_string();
            } else if let Some(rest) = attachment_path(&link.url) {
                // An attachment is written the way the repository stores it,
                // `assets/<page>/<file>`, so that the link also works in a
                // plain clone and in GitHub's own reader. Served from
                // `/page/<id>`, that relative path would resolve to
                // `/page/assets/...`, so it is anchored here. The Markdown on
                // disk is untouched; only the rendered href changes.
                link.url = format!("/assets/{rest}");
            }
        }
    }
    for child in node.children() {
        neutralise_urls(child);
    }
}

/// The `<page>/<file>` tail of a repository-relative attachment URL.
///
/// Only the exact `assets/` prefix counts, and only when it is genuinely
/// relative: a scheme, a leading `/`, or a `//` authority is left alone, and
/// so is anything carrying a `..` component, because rewriting those would be
/// inventing a link the author did not write.
fn attachment_path(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("assets/")?;
    if rest.is_empty() || rest.split('/').any(|c| c == ".." || c.is_empty()) {
        return None;
    }
    Some(rest)
}

/// True for URL schemes that can execute script or smuggle active content.
/// Whitespace and control characters are stripped first so that e.g.
/// `java\tscript:` or a leading newline cannot hide the scheme.
fn is_dangerous_url(url: &str) -> bool {
    let cleaned: String = url
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    cleaned.starts_with("javascript:")
        || cleaned.starts_with("vbscript:")
        // data: URLs are blocked except inline images, which are inert.
        || (cleaned.starts_with("data:") && !cleaned.starts_with("data:image/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_and_paragraphs() {
        let html = render_markdown("# Title\n\nHello **world**.\n");
        assert!(html.contains("<h1>"));
        assert!(html.contains("<strong>world</strong>"));
    }

    #[test]
    fn renders_gfm_tables_and_tasklists() {
        let md = "| A | B |\n| - | - |\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n";
        let html = render_markdown(md);
        assert!(html.contains("<table>"), "GFM table: {html}");
        assert!(html.contains("type=\"checkbox\""), "GFM task list: {html}");
    }

    #[test]
    fn raw_html_block_never_emits_a_live_script() {
        let html = render_markdown("<script>alert('xss')</script>\n\nafter\n");
        // comrak with unsafe_=false drops raw HTML blocks entirely (replacing
        // them with an "omitted" comment) rather than passing them through. The
        // security property we require is simply: no live <script> reaches the
        // browser, and surrounding content still renders.
        assert!(!html.contains("<script>"), "no live script element: {html}");
        assert!(!html.to_lowercase().contains("alert('xss')") || html.contains("omitted"));
        assert!(
            html.contains("after"),
            "surrounding content still renders: {html}"
        );
    }

    #[test]
    fn inline_event_handler_html_cannot_pass_through() {
        let html = render_markdown("<img src=x onerror=alert(1)>\n");
        assert!(
            !html.to_lowercase().contains("onerror=alert"),
            "no live handler: {html}"
        );
    }

    #[test]
    fn javascript_link_is_neutralised() {
        let html = render_markdown("[click](javascript:alert(1))\n");
        assert!(
            !html.contains("javascript:"),
            "dangerous scheme removed: {html}"
        );
        assert!(html.contains("href=\"#\""));
    }

    #[test]
    fn obfuscated_scheme_is_still_caught() {
        // Tab inside the scheme must not smuggle it past the filter.
        let html = render_markdown("[x](java\tscript:alert(1))\n");
        assert!(!html.to_lowercase().contains("javascript:"));
    }

    #[test]
    fn data_image_allowed_but_data_html_blocked() {
        let ok = render_markdown("![i](data:image/png;base64,iVBOR)\n");
        assert!(ok.contains("data:image/png"), "inert data-image kept: {ok}");
        let bad = render_markdown("[x](data:text/html,<script>1</script>)\n");
        assert!(
            !bad.contains("data:text/html"),
            "active data URL removed: {bad}"
        );
    }

    #[test]
    fn normal_relative_and_http_links_survive() {
        let html = render_markdown("[a](Teaching--Course-A) and [b](https://example.com)\n");
        assert!(html.contains("href=\"Teaching--Course-A\""));
        assert!(html.contains("href=\"https://example.com\""));
    }

    #[test]
    fn an_attachment_link_is_anchored_so_it_resolves_from_a_page_route() {
        // The Markdown on disk says `assets/<page>/<file>` because that is
        // where the repository actually stores it, so a plain clone and
        // GitHub's own reader both follow the link. Served from `/page/<id>`
        // a relative path would resolve to `/page/assets/...`, which is not a
        // route, so the rendered href is anchored at the root.
        for (md, want) in [
            (
                "![berry](assets/page-1/berry.png)\n",
                "src=\"/assets/page-1/berry.png\"",
            ),
            (
                "[notes](assets/page-1/notes.pdf)\n",
                "href=\"/assets/page-1/notes.pdf\"",
            ),
        ] {
            let html = render_markdown(md);
            assert!(html.contains(want), "{md:?} -> {html}");
        }
    }

    #[test]
    fn only_the_attachment_prefix_is_rewritten() {
        // A link that merely mentions the word must be left alone, or the
        // rewrite silently breaks ordinary page links.
        for md in [
            "[a](assets)\n",
            "[a](assetsxyz/file.png)\n",
            "[a](my-assets/file.png)\n",
            "[a](https://example.com/assets/x.png)\n",
            "[a](/assets/already-absolute.png)\n",
        ] {
            let html = render_markdown(md);
            assert!(
                !html.contains("href=\"/assets/file.png\"") && !html.contains("//assets/"),
                "{md:?} -> {html}"
            );
        }
        // The last two must survive byte-for-byte as authored.
        assert!(render_markdown("[a](https://example.com/assets/x.png)\n")
            .contains("href=\"https://example.com/assets/x.png\""));
        assert!(render_markdown("[a](/assets/already-absolute.png)\n")
            .contains("href=\"/assets/already-absolute.png\""));
    }

    #[test]
    fn a_traversal_inside_an_attachment_link_is_not_anchored() {
        // Anchoring `assets/../../etc/passwd` would hand the serve layer a
        // path it then has to defend against. It is left as authored instead,
        // so it is an ordinary relative link that resolves to nothing.
        for md in [
            "[a](assets/../secret.png)\n",
            "[a](assets/..)\n",
            "[a](assets//double.png)\n",
            "[a](assets/)\n",
        ] {
            let html = render_markdown(md);
            assert!(
                !html.contains("href=\"/assets/"),
                "not anchored: {md:?} -> {html}"
            );
        }
    }

    #[test]
    fn a_dangerous_scheme_still_wins_over_the_attachment_rewrite() {
        // Order matters: the scheme check runs first, so nothing that looks
        // like an attachment can be used to get past it.
        let html = render_markdown("[x](javascript:alert(1))\n");
        assert!(html.contains("href=\"#\""), "{html}");
    }

    #[test]
    fn rendering_is_deterministic() {
        let md = "# H\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n[x](javascript:1)\n";
        assert_eq!(render_markdown(md), render_markdown(md));
    }
}
