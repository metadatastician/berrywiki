// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Structural accessibility audit over a rendered BerryWiki page.
//!
//! # What this is, and what it is not
//!
//! This crate answers the only part of accessibility a program can answer by
//! itself: whether the *structure* a screen reader and a keyboard depend on is
//! present. It reports a missing `lang`, an unlabelled control, a link with no
//! text, a heading level that jumps, a skip link that points at nothing.
//!
//! It cannot tell you whether the reading order makes sense, whether a label
//! is a *good* label, whether the focus ring is visible against its background,
//! or whether the page is usable. Those need a person, and they are what
//! `docs/execution/a11y-walkthrough.adoc` is for. Nothing here should be read
//! as evidence that BerryWiki has been tested with a screen reader; it has not.
//!
//! Colour contrast is deliberately **out of scope**. Contrast is a property of
//! the stylesheet, not of the document, and computing it here would mean
//! parsing CSS and resolving the cascade. The ratios are recorded by hand in
//! ADR-0012 instead.
//!
//! # Why it is a crate rather than a test helper
//!
//! `berrywiki-serve` sweeps its routes twice: once from a unit test inside
//! `src/lib.rs`, once from the integration test `tests/sync.rs`. An integration
//! test cannot see a `#[cfg(test)]` item in `src/`, so a helper written in
//! either place has to be duplicated into the other. A duplicated allowlist is
//! deliberate — widening what is *served* must not widen the gate in the same
//! edit — but this audit has no such adversarial coupling: it is pure analysis,
//! so two copies would buy nothing and drift apart. One crate, used by both,
//! and gated by its own tests, which is what stops it becoming decorative.
//!
//! It is a dev-dependency. No shipped binary links it.
//!
//! # Parsing
//!
//! The tokeniser is a deliberately small tag scanner, not an HTML parser. It is
//! sound *for this input* because `berrywiki-render` escapes raw HTML
//! (`render.unsafe = false`) and every other surface is built by
//! `berrywiki-serve` from escaped fragments, so every `<` in a BerryWiki
//! response opens a real tag. Point it at arbitrary web HTML and it will be
//! wrong. That is the trade: a hundred lines that we understand completely,
//! against a dependency whose behaviour we would have to trust.

/// One structural defect. `rule` is a stable id so a test can assert *which*
/// rule fired rather than merely that something did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub detail: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.rule, self.detail)
    }
}

fn finding(rule: &'static str, detail: impl Into<String>) -> Finding {
    Finding {
        rule,
        detail: detail.into(),
    }
}

// --- tokeniser -------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Attrs(Vec<(String, String)>);

impl Attrs {
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    /// An attribute counts as an accessible name only when it is non-blank.
    /// `aria-label=""` is the same as no name at all, and is a real mistake.
    fn named_by(&self, key: &str) -> bool {
        self.get(key).is_some_and(|v| !v.trim().is_empty())
    }
}

#[derive(Debug, Clone)]
enum Token {
    Open { name: String, attrs: Attrs },
    Close { name: String },
    Text(String),
}

/// Elements that never have a closing tag, so an `Open` for one must not be
/// treated as containing everything after it.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':'
}

fn tokenize(html: &str) -> Vec<Token> {
    let c: Vec<char> = html.chars().collect();
    let n = c.len();
    let mut out = Vec::new();
    let mut text = String::new();
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !text.is_empty() {
                out.push(Token::Text(std::mem::take(&mut text)));
            }
        };
    }

    while i < n {
        if c[i] != '<' {
            text.push(c[i]);
            i += 1;
            continue;
        }
        // `<!-- ... -->` and `<!doctype ...>` carry no semantics for us.
        if c[i..].starts_with(&['<', '!', '-', '-']) {
            flush!();
            i += 4;
            while i < n && !c[i..].starts_with(&['-', '-', '>']) {
                i += 1;
            }
            i = (i + 3).min(n);
            continue;
        }
        if c[i..].starts_with(&['<', '!']) {
            flush!();
            while i < n && c[i] != '>' {
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }
        if c[i..].starts_with(&['<', '/']) {
            flush!();
            i += 2;
            let start = i;
            while i < n && is_name_char(c[i]) {
                i += 1;
            }
            let name: String = c[start..i].iter().collect::<String>().to_lowercase();
            while i < n && c[i] != '>' {
                i += 1;
            }
            i = (i + 1).min(n);
            if !name.is_empty() {
                out.push(Token::Close { name });
            }
            continue;
        }
        if i + 1 < n && c[i + 1].is_ascii_alphabetic() {
            flush!();
            i += 1;
            let start = i;
            while i < n && is_name_char(c[i]) {
                i += 1;
            }
            let name: String = c[start..i].iter().collect::<String>().to_lowercase();
            let mut attrs = Attrs::default();
            loop {
                while i < n && c[i].is_whitespace() {
                    i += 1;
                }
                if i >= n || c[i] == '>' {
                    i = (i + 1).min(n);
                    break;
                }
                if c[i] == '/' {
                    i += 1;
                    continue;
                }
                let ks = i;
                while i < n && is_name_char(c[i]) {
                    i += 1;
                }
                if i == ks {
                    // Not an attribute name; skip a character so we cannot spin.
                    i += 1;
                    continue;
                }
                let key: String = c[ks..i].iter().collect::<String>().to_lowercase();
                while i < n && c[i].is_whitespace() {
                    i += 1;
                }
                let mut value = String::new();
                if i < n && c[i] == '=' {
                    i += 1;
                    while i < n && c[i].is_whitespace() {
                        i += 1;
                    }
                    if i < n && (c[i] == '"' || c[i] == '\'') {
                        let q = c[i];
                        i += 1;
                        while i < n && c[i] != q {
                            value.push(c[i]);
                            i += 1;
                        }
                        i = (i + 1).min(n);
                    } else {
                        while i < n && !c[i].is_whitespace() && c[i] != '>' {
                            value.push(c[i]);
                            i += 1;
                        }
                    }
                }
                attrs.0.push((key, value));
            }
            out.push(Token::Open { name, attrs });
            continue;
        }
        // A bare `<` that opens nothing. Text.
        text.push('<');
        i += 1;
    }
    flush!();
    out
}

/// The text a screen reader would announce for the element opened at `start`.
///
/// Text content plus the `alt` of any descendant image, which is what makes an
/// icon-only link accessible. Stops at the matching close tag, counting nesting
/// so an inner `<a>` inside an `<a>` cannot end the outer one early.
fn accessible_text(tokens: &[Token], start: usize) -> String {
    let Token::Open { name, .. } = &tokens[start] else {
        return String::new();
    };
    if VOID.contains(&name.as_str()) {
        return String::new();
    }
    let mut depth = 0usize;
    let mut out = String::new();
    for tok in &tokens[start + 1..] {
        match tok {
            Token::Open { name: n, attrs } => {
                if n == name {
                    depth += 1;
                }
                if n == "img" {
                    if let Some(alt) = attrs.get("alt") {
                        out.push_str(alt);
                    }
                }
            }
            Token::Close { name: n } => {
                if n == name {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
            }
            Token::Text(t) => out.push_str(t),
        }
    }
    out
}

// --- the rules -------------------------------------------------------------

/// Audit a **complete** HTML document.
///
/// Document-level rules are unconditional on purpose: an emitter that stopped
/// producing `<html>` would otherwise silently switch four rules off, which is
/// the shape of a gate that cannot fail. Call this on whole responses only.
pub fn audit(html: &str) -> Vec<Finding> {
    let t = tokenize(html);
    let mut f = Vec::new();

    document_rules(&t, &mut f);
    heading_rules(&t, &mut f);
    landmark_rules(&t, &mut f);
    control_rules(&t, &mut f);
    interactive_rules(&t, &mut f);
    id_rules(&t, &mut f);
    f
}

fn opens<'a>(t: &'a [Token], want: &str) -> Vec<(usize, &'a Attrs)> {
    t.iter()
        .enumerate()
        .filter_map(|(i, tok)| match tok {
            Token::Open { name, attrs } if name == want => Some((i, attrs)),
            _ => None,
        })
        .collect()
}

fn document_rules(t: &[Token], f: &mut Vec<Finding>) {
    match opens(t, "html").first() {
        None => f.push(finding(
            "a11y.lang",
            "no <html> element, so no document language",
        )),
        Some((_, a)) => {
            if !a.named_by("lang") {
                f.push(finding(
                    "a11y.lang",
                    "<html> has no non-empty lang attribute",
                ));
            }
        }
    }

    match opens(t, "title").first() {
        None => f.push(finding("a11y.title", "no <title> element")),
        Some((i, _)) => {
            if accessible_text(t, *i).trim().is_empty() {
                f.push(finding("a11y.title", "<title> is empty"));
            }
        }
    }

    // The skip link: the first focusable thing on the page must be a way past
    // the navigation, and it must point at something that exists.
    let ids = all_ids(t);
    let first_link = opens(t, "a")
        .into_iter()
        .find(|(_, a)| a.get("href").is_some());
    match first_link {
        None => f.push(finding(
            "a11y.skip-link",
            "no links at all, so no skip link",
        )),
        Some((i, a)) => {
            let href = a.get("href").unwrap_or_default();
            let text = accessible_text(t, i);
            match href.strip_prefix('#') {
                None => f.push(finding(
                    "a11y.skip-link",
                    format!(
                        "the first focusable link is {href:?} ({:?}), not a skip link",
                        text.trim()
                    ),
                )),
                Some("") => f.push(finding("a11y.skip-link", "skip link href is bare '#'")),
                Some(target) => {
                    if !ids.iter().any(|id| id == target) {
                        f.push(finding(
                            "a11y.skip-link",
                            format!("skip link targets #{target}, which no element has"),
                        ));
                    }
                    if text.trim().is_empty() {
                        f.push(finding("a11y.skip-link", "skip link has no text"));
                    }
                }
            }
        }
    }
}

fn heading_rules(t: &[Token], f: &mut Vec<Finding>) {
    let mut levels: Vec<(usize, u8)> = Vec::new();
    for (i, tok) in t.iter().enumerate() {
        if let Token::Open { name, .. } = tok {
            if name.len() == 2 && name.starts_with('h') {
                if let Some(d) = name[1..].parse::<u8>().ok().filter(|d| (1..=6).contains(d)) {
                    levels.push((i, d));
                }
            }
        }
    }

    let h1s = levels.iter().filter(|(_, d)| *d == 1).count();
    if h1s != 1 {
        let titles: Vec<String> = levels
            .iter()
            .filter(|(_, d)| *d == 1)
            .map(|(i, _)| accessible_text(t, *i).trim().to_string())
            .collect();
        f.push(finding(
            "a11y.h1-count",
            format!("expected exactly one <h1>, found {h1s} {titles:?}"),
        ));
    }

    for w in levels.windows(2) {
        let (_, prev) = w[0];
        let (i, next) = w[1];
        if next > prev + 1 {
            f.push(finding(
                "a11y.heading-skip",
                format!(
                    "h{prev} is followed by h{next} ({:?}); a level was skipped",
                    accessible_text(t, i).trim()
                ),
            ));
        }
    }
}

fn landmark_rules(t: &[Token], f: &mut Vec<Finding>) {
    // A landmark a reader can jump to needs a name to tell it from its
    // siblings. `main` is exempt: there is one per document by definition.
    for tag in ["nav", "aside"] {
        for (_, a) in opens(t, tag) {
            if !a.named_by("aria-label") && !a.named_by("aria-labelledby") {
                f.push(finding(
                    "a11y.landmark-name",
                    format!("<{tag}> has no aria-label or aria-labelledby"),
                ));
            }
        }
    }
    let mains = opens(t, "main").len();
    if mains != 1 {
        f.push(finding(
            "a11y.main",
            format!("expected exactly one <main>, found {mains}"),
        ));
    }
}

fn control_rules(t: &[Token], f: &mut Vec<Finding>) {
    // A control is labelled by a <label for>, or by aria-label/-labelledby.
    // `title` and placeholder text are not accepted: neither is announced
    // dependably, and both vanish the moment the field has a value.
    let labelled: Vec<String> = opens(t, "label")
        .into_iter()
        .filter_map(|(_, a)| a.get("for").map(str::to_string))
        .collect();

    for tag in ["input", "textarea", "select"] {
        for (_, a) in opens(t, tag) {
            // A disabled control is out of the tab order and cannot be
            // operated, so there is nothing for a label to name. This is not a
            // loophole in the abstract: GFM task lists render as
            // `<input type="checkbox" disabled>` followed by the item text, and
            // that is byte-for-byte how GitHub renders the same Markdown.
            // Labelling them would mean diverging from the native reader to
            // satisfy a rule that does not apply to an inert control.
            if a.get("disabled").is_some() {
                continue;
            }
            let kind = a.get("type").unwrap_or("text").to_lowercase();
            if tag == "input" && matches!(kind.as_str(), "hidden" | "submit" | "reset" | "button") {
                continue;
            }
            let by_label = a
                .get("id")
                .is_some_and(|id| labelled.iter().any(|l| l == id));
            if !by_label && !a.named_by("aria-label") && !a.named_by("aria-labelledby") {
                f.push(finding(
                    "a11y.control-label",
                    format!(
                        "<{tag}{}> has no label: no <label for>, no aria-label",
                        a.get("name")
                            .map(|n| format!(" name={n:?}"))
                            .unwrap_or_default()
                    ),
                ));
            }
        }
    }
}

fn interactive_rules(t: &[Token], f: &mut Vec<Finding>) {
    for (i, a) in opens(t, "a") {
        if a.get("href").is_none() {
            continue; // Not a link and not focusable; nothing to announce.
        }
        if accessible_text(t, i).trim().is_empty() && !a.named_by("aria-label") {
            f.push(finding(
                "a11y.link-text",
                format!(
                    "<a href={:?}> has no text",
                    a.get("href").unwrap_or_default()
                ),
            ));
        }
    }
    for (i, a) in opens(t, "button") {
        if accessible_text(t, i).trim().is_empty() && !a.named_by("aria-label") {
            f.push(finding("a11y.button-text", "<button> has no text"));
        }
    }
    for (_, a) in opens(t, "img") {
        if a.get("alt").is_none() {
            f.push(finding(
                "a11y.img-alt",
                format!(
                    "<img src={:?}> has no alt attribute",
                    a.get("src").unwrap_or_default()
                ),
            ));
        }
    }
    for tok in t {
        if let Token::Open { name, attrs } = tok {
            if let Some(v) = attrs.get("tabindex") {
                if v.trim().parse::<i32>().is_ok_and(|n| n > 0) {
                    f.push(finding(
                        "a11y.positive-tabindex",
                        format!("<{name} tabindex={v:?}> overrides document order"),
                    ));
                }
            }
            // ADR-0012: never claim a widget role that only script can honour.
            if let Some(role) = attrs.get("role") {
                if matches!(role, "tab" | "tablist" | "tabpanel") {
                    f.push(finding(
                        "a11y.fake-widget",
                        format!(
                            "<{name} role={role:?}> promises keyboard behaviour no script provides"
                        ),
                    ));
                }
            }
        }
    }
}

fn all_ids(t: &[Token]) -> Vec<String> {
    t.iter()
        .filter_map(|tok| match tok {
            Token::Open { attrs, .. } => attrs.get("id").map(str::to_string),
            _ => None,
        })
        .filter(|id| !id.is_empty())
        .collect()
}

fn id_rules(t: &[Token], f: &mut Vec<Finding>) {
    let ids = all_ids(t);
    let mut seen: Vec<&String> = Vec::new();
    let mut reported: Vec<&String> = Vec::new();
    for id in &ids {
        if seen.contains(&id) && !reported.contains(&id) {
            reported.push(id);
            f.push(finding(
                "a11y.duplicate-id",
                format!("id {id:?} is used more than once"),
            ));
        }
        seen.push(id);
    }
}
