// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Every rule is proved twice: once by the positive control below, which must
//! produce no findings at all, and once by a minimal mutation of it that must
//! produce exactly that rule. A rule with no failing case is decorative, and a
//! rule that fires on the clean document would make the gate unusable.

use berrywiki_a11y::audit;

/// A small but complete document exercising every rule's *passing* side.
const GOOD: &str = "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>A page — BerryWiki</title></head><body>\
<a class=\"skip\" href=\"#content\">Skip to content</a>\
<header><a class=\"brand\" href=\"/\">BerryWiki</a>\
<form method=\"get\" action=\"/search\" role=\"search\">\
<input type=\"search\" name=\"q\" aria-label=\"Search\">\
<button type=\"submit\">Search</button></form></header>\
<nav aria-label=\"Notebook\"><ul><li><a href=\"/page/x\">X</a></li></ul></nav>\
<main id=\"content\"><h1>A page</h1><h2>A section</h2><p>Body.</p>\
<img src=\"/assets/x/berry.png\" alt=\"A berry\">\
<form method=\"post\"><label for=\"t\">Title</label>\
<input type=\"text\" id=\"t\" name=\"title\">\
<input type=\"hidden\" name=\"hash\" value=\"abc\">\
<button type=\"submit\">Save</button></form></main>\
<aside aria-label=\"Page context\"><h2>Backlinks</h2></aside>\
</body></html>";

fn rules(html: &str) -> Vec<&'static str> {
    let mut r: Vec<&'static str> = audit(html).into_iter().map(|f| f.rule).collect();
    r.sort_unstable();
    r.dedup();
    r
}

#[test]
fn the_positive_control_is_clean() {
    let found = audit(GOOD);
    assert!(
        found.is_empty(),
        "the good document should be clean, got: {found:?}"
    );
}

/// Mutate `GOOD` and assert the named rule, and only new findings of that kind,
/// appear. Returns nothing; it panics with the diff on failure.
fn plant(from: &str, to: &str, rule: &str) {
    assert!(
        GOOD.contains(from),
        "test is stale: {from:?} is no longer in the document"
    );
    let mutated = GOOD.replacen(from, to, 1);
    let found = rules(&mutated);
    assert!(
        found.contains(&rule),
        "planting {from:?} -> {to:?} should have raised {rule}, raised {found:?}"
    );
}

#[test]
fn a_missing_language_is_caught() {
    plant("<html lang=\"en\">", "<html>", "a11y.lang");
}

#[test]
fn an_empty_language_is_caught() {
    plant("<html lang=\"en\">", "<html lang=\"\">", "a11y.lang");
}

#[test]
fn a_missing_title_is_caught() {
    plant("<title>A page — BerryWiki</title>", "", "a11y.title");
}

#[test]
fn an_empty_title_is_caught() {
    plant(
        "<title>A page — BerryWiki</title>",
        "<title></title>",
        "a11y.title",
    );
}

#[test]
fn a_missing_skip_link_is_caught() {
    plant(
        "<a class=\"skip\" href=\"#content\">Skip to content</a>",
        "",
        "a11y.skip-link",
    );
}

#[test]
fn a_skip_link_pointing_at_nothing_is_caught() {
    plant(
        "href=\"#content\">Skip",
        "href=\"#nowhere\">Skip",
        "a11y.skip-link",
    );
}

#[test]
fn a_skip_link_with_no_text_is_caught() {
    plant(">Skip to content</a>", "></a>", "a11y.skip-link");
}

#[test]
fn a_second_h1_is_caught() {
    plant("<h2>A section</h2>", "<h1>A section</h1>", "a11y.h1-count");
}

#[test]
fn no_h1_at_all_is_caught() {
    plant("<h1>A page</h1>", "<p>A page</p>", "a11y.h1-count");
}

#[test]
fn a_skipped_heading_level_is_caught() {
    plant(
        "<h2>A section</h2>",
        "<h3>A section</h3>",
        "a11y.heading-skip",
    );
}

#[test]
fn an_unnamed_nav_is_caught() {
    plant(
        "<nav aria-label=\"Notebook\">",
        "<nav>",
        "a11y.landmark-name",
    );
}

#[test]
fn a_blank_named_nav_is_caught() {
    plant(
        "<nav aria-label=\"Notebook\">",
        "<nav aria-label=\" \">",
        "a11y.landmark-name",
    );
}

#[test]
fn an_unnamed_aside_is_caught() {
    plant(
        "<aside aria-label=\"Page context\">",
        "<aside>",
        "a11y.landmark-name",
    );
}

#[test]
fn a_missing_main_is_caught() {
    plant("<main id=\"content\">", "<div id=\"content\">", "a11y.main");
}

#[test]
fn an_unlabelled_search_input_is_caught() {
    plant(
        "<input type=\"search\" name=\"q\" aria-label=\"Search\">",
        "<input type=\"search\" name=\"q\">",
        "a11y.control-label",
    );
}

#[test]
fn a_label_whose_for_misses_is_caught() {
    plant(
        "<label for=\"t\">Title</label>",
        "<label for=\"other\">Title</label>",
        "a11y.control-label",
    );
}

#[test]
fn a_placeholder_is_not_a_label() {
    plant(
        "<input type=\"search\" name=\"q\" aria-label=\"Search\">",
        "<input type=\"search\" name=\"q\" placeholder=\"Search…\" title=\"Search\">",
        "a11y.control-label",
    );
}

#[test]
fn a_link_with_no_text_is_caught() {
    plant(
        "<a href=\"/page/x\">X</a>",
        "<a href=\"/page/x\"></a>",
        "a11y.link-text",
    );
}

#[test]
fn an_icon_link_is_accessible_through_its_image_alt() {
    let mutated = GOOD.replacen(
        "<a href=\"/page/x\">X</a>",
        "<a href=\"/page/x\"><img src=\"/i.png\" alt=\"X\"></a>",
        1,
    );
    assert!(
        audit(&mutated).is_empty(),
        "alt text should name the link: {:?}",
        audit(&mutated)
    );
}

#[test]
fn a_button_with_no_text_is_caught() {
    plant(
        "<button type=\"submit\">Search</button>",
        "<button type=\"submit\"></button>",
        "a11y.button-text",
    );
}

#[test]
fn an_image_with_no_alt_is_caught() {
    plant(
        "<img src=\"/assets/x/berry.png\" alt=\"A berry\">",
        "<img src=\"/assets/x/berry.png\">",
        "a11y.img-alt",
    );
}

#[test]
fn an_empty_alt_is_allowed_because_decorative_images_are_real() {
    let mutated = GOOD.replacen("alt=\"A berry\"", "alt=\"\"", 1);
    assert!(
        audit(&mutated).is_empty(),
        "empty alt marks a decorative image: {:?}",
        audit(&mutated)
    );
}

#[test]
fn a_positive_tabindex_is_caught() {
    plant(
        "<main id=\"content\">",
        "<main id=\"content\" tabindex=\"1\">",
        "a11y.positive-tabindex",
    );
}

#[test]
fn tabindex_minus_one_is_allowed() {
    let mutated = GOOD.replacen(
        "<main id=\"content\">",
        "<main id=\"content\" tabindex=\"-1\">",
        1,
    );
    assert!(
        audit(&mutated).is_empty(),
        "-1 is a legitimate programmatic focus target"
    );
}

#[test]
fn a_fake_tablist_is_caught() {
    plant(
        "<nav aria-label=\"Notebook\">",
        "<nav aria-label=\"Notebook\" role=\"tablist\">",
        "a11y.fake-widget",
    );
}

#[test]
fn a_duplicate_id_is_caught() {
    plant(
        "<input type=\"text\" id=\"t\" name=\"title\">",
        "<input type=\"text\" id=\"content\" name=\"title\">",
        "a11y.duplicate-id",
    );
}

// --- tokeniser behaviour the rules depend on -------------------------------

#[test]
fn escaped_markup_in_text_is_not_read_as_a_tag() {
    // What a page containing a literal "<script>" looks like after escaping.
    let mutated = GOOD.replacen(
        "<p>Body.</p>",
        "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>",
        1,
    );
    assert!(
        audit(&mutated).is_empty(),
        "escaped text is text: {:?}",
        audit(&mutated)
    );
}

#[test]
fn a_comment_containing_a_tag_is_ignored() {
    let mutated = GOOD.replacen(
        "<p>Body.</p>",
        "<!-- <h1>not a heading</h1> --><p>Body.</p>",
        1,
    );
    assert!(
        audit(&mutated).is_empty(),
        "comments carry no structure: {:?}",
        audit(&mutated)
    );
}

#[test]
fn a_nested_link_does_not_end_its_parent_early() {
    // Invalid HTML, but the tokeniser must not mis-attribute text because of it.
    let mutated = GOOD.replacen(
        "<a href=\"/page/x\">X</a>",
        "<a href=\"/page/x\"><a href=\"/page/y\">Y</a></a>",
        1,
    );
    assert!(
        audit(&mutated).is_empty(),
        "both links have text: {:?}",
        audit(&mutated)
    );
}

#[test]
fn an_unterminated_tag_does_not_hang_or_panic() {
    for bad in [
        "<html lang",
        "<a href=\"",
        "<!--",
        "<",
        "<<<>>>",
        "</",
        "<a =b>",
    ] {
        let _ = audit(bad); // Must return, and must not panic.
    }
}

#[test]
fn a_disabled_control_needs_no_label() {
    // How comrak renders a GFM task list, which is how GitHub renders it too.
    let html = GOOD.replacen(
        "<p>Body.</p>",
        "<p>Body.</p><ul><li><input type=\"checkbox\" disabled=\"\" /> Done</li></ul>",
        1,
    );
    assert!(html.contains("checkbox"), "test is stale: the anchor moved");
    assert_eq!(rules(&html), Vec::<&str>::new());
}

#[test]
fn an_enabled_control_still_needs_a_label() {
    // The exemption above is for inert controls only; removing `disabled`
    // must bring the rule straight back, or it is a hole rather than a rule.
    let html = GOOD.replacen(
        "<p>Body.</p>",
        "<p>Body.</p><ul><li><input type=\"checkbox\" /> Done</li></ul>",
        1,
    );
    assert!(
        rules(&html).contains(&"a11y.control-label"),
        "{:?}",
        rules(&html)
    );
}
