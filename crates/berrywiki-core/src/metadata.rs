//! The hidden BerryWiki metadata block.
//!
//! A BerryWiki-managed page *may* begin with an HTML comment that is invisible
//! in GitHub's rendered wiki (verified requirement: HTML comments are stripped
//! from rendered output). Example:
//!
//! ```text
//! <!-- berrywiki
//! id: 0195f6ec-36a2-7a42-b519-5f558842e256
//! parent: 0195f6d0-b787-7c3a-a48f-c1a04fb2ea84
//! position: 30
//! kind: page
//! tags:
//!   - assessment
//!   - teaching
//! archived: false
//! -->
//!
//! # Assessment Plan
//! ...
//! ```
//!
//! Design guarantees (from the metadata rules in the spec):
//! * Pages *without* a block parse normally (`metadata == None`).
//! * Malformed metadata never makes a page unreadable — it produces a
//!   [`Diagnostic`] and best-effort defaults.
//! * Unknown top-level keys are preserved verbatim and re-emitted (`extra`).
//! * Serialisation is deterministic and **idempotent**: re-serialising an
//!   unchanged model yields byte-identical output, so re-saving does not create
//!   a meaningless diff.
//!
//! BerryWiki normalises the block on the first *managed* write; we do not try
//! to preserve arbitrary input whitespace byte-for-byte (documented in
//! ADR-0004 metadata format).

use crate::diagnostics::Diagnostic;

const OPEN_MARKER: &str = "<!-- berrywiki";
const CLOSE_MARKER: &str = "-->";

/// The kind of a page. Extensible; unknown kinds are preserved as-is via a
/// diagnostic + fallback to `Page` so the file stays readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageKind {
    Page,
    Code,
    /// An unrecognised kind string, preserved verbatim.
    Other(String),
}

impl PageKind {
    fn parse(s: &str) -> Self {
        match s {
            "page" => PageKind::Page,
            "code" => PageKind::Code,
            other => PageKind::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            PageKind::Page => "page",
            PageKind::Code => "code",
            PageKind::Other(s) => s,
        }
    }
}

/// Parsed BerryWiki metadata.
///
/// `extra` holds unknown top-level `key: value` lines verbatim (without a
/// trailing newline), so they survive a parse → serialise round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMetadata {
    pub id: String,
    pub parent_id: Option<String>,
    pub position: i64,
    pub kind: PageKind,
    pub tags: Vec<String>,
    pub archived: bool,
    pub extra: Vec<String>,
}

impl PageMetadata {
    /// A new managed page with sensible defaults.
    pub fn new(id: impl Into<String>) -> Self {
        PageMetadata {
            id: id.into(),
            parent_id: None,
            position: 0,
            kind: PageKind::Page,
            tags: Vec::new(),
            archived: false,
            extra: Vec::new(),
        }
    }
}

/// Result of splitting a page source into its (optional) metadata block and body.
#[derive(Debug, Clone)]
pub struct ParsedSource {
    pub metadata: Option<PageMetadata>,
    /// The Markdown body with the metadata block removed. If there was no
    /// block, this is the original source unchanged.
    pub body: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// Split a raw page source into metadata + body, collecting diagnostics.
///
/// The block is only recognised when it is the first non-blank content of the
/// file (GitHub renders a leading HTML comment invisibly; a block buried
/// mid-page would be a content comment, not our metadata).
pub fn parse_source(source: &str) -> ParsedSource {
    let mut diagnostics = Vec::new();

    // Locate the opening marker, allowing only leading blank lines before it.
    let leading_ok = source
        .lines()
        .take_while(|l| l.trim().is_empty() || l.trim_start().starts_with(OPEN_MARKER))
        .any(|l| l.trim_start().starts_with(OPEN_MARKER));

    if !leading_ok {
        return ParsedSource {
            metadata: None,
            body: source.to_string(),
            diagnostics,
        };
    }

    let lines: Vec<&str> = source.lines().collect();
    let open_idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with(OPEN_MARKER));
    let Some(open_idx) = open_idx else {
        return ParsedSource {
            metadata: None,
            body: source.to_string(),
            diagnostics,
        };
    };

    let close_rel = lines[open_idx + 1..]
        .iter()
        .position(|l| l.trim() == CLOSE_MARKER);
    let Some(close_rel) = close_rel else {
        // Unterminated block: keep the page readable, warn, treat as no metadata.
        diagnostics.push(Diagnostic::warning(
            "metadata.unterminated",
            "BerryWiki metadata block was opened but never closed with `-->`; \
             the block was ignored and left in the page body.",
        ));
        return ParsedSource {
            metadata: None,
            body: source.to_string(),
            diagnostics,
        };
    };
    let close_idx = open_idx + 1 + close_rel;

    let meta = parse_block(&lines[open_idx + 1..close_idx], &mut diagnostics);

    // Body = everything after the close marker, dropping exactly one blank
    // separator line if present, so a normalised block does not accumulate
    // blank lines on each round-trip.
    let mut body_start = close_idx + 1;
    if lines.get(body_start).map(|l| l.trim().is_empty()) == Some(true) {
        body_start += 1;
    }
    let body = lines[body_start..].join("\n");
    // Preserve a single trailing newline convention for the body.
    let body = if body.is_empty() {
        String::new()
    } else {
        format!("{body}\n")
    };

    ParsedSource {
        metadata: Some(meta),
        body,
        diagnostics,
    }
}

fn parse_block(lines: &[&str], diagnostics: &mut Vec<Diagnostic>) -> PageMetadata {
    let mut meta = PageMetadata::new(String::new());
    let mut seen_id = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let Some((key, value)) = split_key_value(line) else {
            diagnostics.push(Diagnostic::warning(
                "metadata.unparsed-line",
                format!("Ignored unparseable metadata line: {:?}", line.trim()),
            ));
            i += 1;
            continue;
        };
        match key.as_str() {
            "id" => {
                meta.id = value.to_string();
                seen_id = true;
            }
            "parent" => {
                meta.parent_id = if value == "null" || value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
            "position" => match value.parse::<i64>() {
                Ok(p) => meta.position = p,
                Err(_) => diagnostics.push(Diagnostic::warning(
                    "metadata.bad-position",
                    format!("Invalid `position` value {value:?}; defaulted to 0."),
                )),
            },
            "kind" => meta.kind = PageKind::parse(value),
            "archived" => match value {
                "true" => meta.archived = true,
                "false" => meta.archived = false,
                _ => diagnostics.push(Diagnostic::warning(
                    "metadata.bad-archived",
                    format!("Invalid `archived` value {value:?}; defaulted to false."),
                )),
            },
            "tags" => {
                if value == "[]" {
                    meta.tags = Vec::new();
                } else if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']'))
                {
                    // Inline list form: tags: [a, b]
                    meta.tags = inner
                        .split(',')
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                } else {
                    // Block list form: following `  - item` lines.
                    let mut tags = Vec::new();
                    while i + 1 < lines.len() {
                        let next = lines[i + 1];
                        if let Some(item) = next.trim_start().strip_prefix("- ") {
                            tags.push(item.trim().to_string());
                            i += 1;
                        } else if next.trim().is_empty() {
                            break;
                        } else {
                            break;
                        }
                    }
                    meta.tags = tags;
                }
            }
            _ => {
                // Unknown top-level key: preserve verbatim, INCLUDING any
                // block-list items or indented continuation lines beneath it.
                // Dropping them would silently corrupt the file on the next
                // save (breaking the unknown-field-preservation contract).
                meta.extra.push(line.trim_end().to_string());
                while i + 1 < lines.len() {
                    let next = lines[i + 1];
                    let indented = next.starts_with(' ') || next.starts_with('\t');
                    if indented && !next.trim().is_empty() {
                        meta.extra.push(next.trim_end().to_string());
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        i += 1;
    }

    if !seen_id {
        diagnostics.push(Diagnostic::warning(
            "metadata.missing-id",
            "BerryWiki metadata block has no `id`; the page cannot be managed \
             (moved, backlinked) reliably until an id is assigned.",
        ));
    }
    meta
}

/// Split `key: value`, tolerating leading indentation. Returns `None` for lines
/// that are not a key/value pair (e.g. stray list items).
fn split_key_value(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") {
        return None;
    }
    let (k, v) = trimmed.split_once(':')?;
    let key = k.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    Some((key.to_string(), v.trim()))
}

/// Neutralise any field content that could break the HTML-comment block:
/// line breaks (which would inject spurious metadata lines) and the literal
/// close marker `-->` (which would terminate the block early). This is a
/// last-resort gate — callers should validate input first (see
/// [`sanitises_field`]) — but it guarantees a well-formed, re-parseable block
/// no matter what a field contains, so page content can never be corrupted by
/// a stray tag. It only ever alters pathological values.
fn sanitise_field(value: &str) -> String {
    let mut s: String = value
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c.is_control() { ' ' } else { c })
        .collect();
    if s.contains("-->") {
        s = s.replace("-->", "-- >");
    }
    s
}

/// True when serialising `value` would require sanitisation (i.e. it contains
/// a newline, control character or the `-->` token). Callers use this to
/// reject bad input with a clear error rather than silently rewriting it.
pub fn sanitises_field(value: &str) -> bool {
    value.chars().any(|c| c == '\n' || c == '\r' || c.is_control()) || value.contains("-->")
}

/// Serialise metadata into the canonical, deterministic block form (including
/// the open/close markers and a trailing newline). Key order is fixed. Field
/// values are sanitised so the block is always well-formed and re-parseable.
pub fn serialize_metadata(meta: &PageMetadata) -> String {
    let mut out = String::new();
    out.push_str(OPEN_MARKER);
    out.push('\n');
    out.push_str(&format!("id: {}\n", sanitise_field(&meta.id)));
    match &meta.parent_id {
        Some(p) => out.push_str(&format!("parent: {}\n", sanitise_field(p))),
        None => out.push_str("parent: null\n"),
    }
    out.push_str(&format!("position: {}\n", meta.position));
    out.push_str(&format!("kind: {}\n", sanitise_field(meta.kind.as_str())));
    if meta.tags.is_empty() {
        out.push_str("tags: []\n");
    } else {
        out.push_str("tags:\n");
        for tag in &meta.tags {
            out.push_str(&format!("  - {}\n", sanitise_field(tag)));
        }
    }
    out.push_str(&format!("archived: {}\n", meta.archived));
    for line in &meta.extra {
        // Extra lines carry their own indentation; only neutralise a stray
        // close marker, never their leading whitespace.
        let safe = if line.contains("-->") {
            line.replace("-->", "-- >")
        } else {
            line.clone()
        };
        out.push_str(safe.trim_end_matches(['\n', '\r']));
        out.push('\n');
    }
    out.push_str(CLOSE_MARKER);
    out.push('\n');
    out
}

/// Re-assemble a full page source from metadata + body in canonical form.
pub fn serialize_source(meta: Option<&PageMetadata>, body: &str) -> String {
    match meta {
        None => body.to_string(),
        Some(m) => {
            let block = serialize_metadata(m);
            let body_trimmed = body.trim_start_matches('\n');
            if body_trimmed.is_empty() {
                block
            } else {
                format!("{block}\n{body_trimmed}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "<!-- berrywiki\n\
id: 0195f6ec-36a2-7a42-b519-5f558842e256\n\
parent: 0195f6d0-b787-7c3a-a48f-c1a04fb2ea84\n\
position: 30\n\
kind: page\n\
tags:\n\
  - assessment\n\
  - teaching\n\
archived: false\n\
-->\n\
\n\
# Assessment Plan\n\
\n\
This page describes the assessment strategy.\n";

    #[test]
    fn parses_full_block() {
        let parsed = parse_source(SAMPLE);
        let m = parsed.metadata.expect("metadata present");
        assert_eq!(m.id, "0195f6ec-36a2-7a42-b519-5f558842e256");
        assert_eq!(m.parent_id.as_deref(), Some("0195f6d0-b787-7c3a-a48f-c1a04fb2ea84"));
        assert_eq!(m.position, 30);
        assert_eq!(m.kind, PageKind::Page);
        assert_eq!(m.tags, vec!["assessment", "teaching"]);
        assert!(!m.archived);
        assert!(parsed.body.starts_with("# Assessment Plan"));
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn page_without_metadata_opens_normally() {
        let src = "# Just A Page\n\nHello.\n";
        let parsed = parse_source(src);
        assert!(parsed.metadata.is_none());
        assert_eq!(parsed.body, src);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn round_trip_is_stable() {
        let parsed = parse_source(SAMPLE);
        let m = parsed.metadata.unwrap();
        // Model survives parse -> serialise -> parse.
        let reserialised = serialize_metadata(&m);
        let reparsed = parse_source(&format!("{reserialised}\n{}", parsed.body));
        assert_eq!(reparsed.metadata.unwrap(), m);
    }

    #[test]
    fn serialisation_is_idempotent() {
        let parsed = parse_source(SAMPLE);
        let m = parsed.metadata.unwrap();
        let once = serialize_metadata(&m);
        let twice = serialize_metadata(&parse_source(&once).metadata.unwrap());
        assert_eq!(once, twice);
    }

    #[test]
    fn unknown_fields_are_preserved() {
        let src = "<!-- berrywiki\nid: x\nicon: rocket\ncolor: blue\n-->\n\n# T\n";
        let parsed = parse_source(src);
        let m = parsed.metadata.unwrap();
        assert!(m.extra.contains(&"icon: rocket".to_string()));
        assert!(m.extra.contains(&"color: blue".to_string()));
        let out = serialize_metadata(&m);
        assert!(out.contains("icon: rocket"));
        assert!(out.contains("color: blue"));
    }

    #[test]
    fn malformed_position_warns_but_survives() {
        let src = "<!-- berrywiki\nid: x\nposition: not-a-number\n-->\n\n# T\n";
        let parsed = parse_source(src);
        let m = parsed.metadata.unwrap();
        assert_eq!(m.position, 0);
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.code == "metadata.bad-position"));
    }

    #[test]
    fn unterminated_block_is_non_fatal() {
        let src = "<!-- berrywiki\nid: x\n\n# Title still readable\n";
        let parsed = parse_source(src);
        assert!(parsed.metadata.is_none());
        assert!(parsed.body.contains("# Title still readable"));
        assert!(parsed
            .diagnostics
            .iter()
            .any(|d| d.code == "metadata.unterminated"));
    }

    #[test]
    fn empty_tags_round_trip_as_inline() {
        let mut m = PageMetadata::new("x");
        m.tags = vec![];
        let out = serialize_metadata(&m);
        assert!(out.contains("tags: []"));
        assert_eq!(parse_source(&out).metadata.unwrap().tags, Vec::<String>::new());
    }

    #[test]
    fn unknown_kind_preserved() {
        let src = "<!-- berrywiki\nid: x\nkind: template\n-->\n\n# T\n";
        let m = parse_source(src).metadata.unwrap();
        assert_eq!(m.kind, PageKind::Other("template".to_string()));
        assert!(serialize_metadata(&m).contains("kind: template"));
    }

    #[test]
    fn multiline_unknown_metadata_survives_round_trip() {
        // Review finding #4: an unknown key with a block list must not lose its
        // items on re-serialisation.
        let src = "<!-- berrywiki\nid: x\ncollaborators:\n  - alice\n  - bob\ncolor: blue\n-->\n\n# T\n";
        let parsed = parse_source(src);
        let m = parsed.metadata.unwrap();
        let out = serialize_metadata(&m);
        assert!(out.contains("collaborators:"), "unknown key kept");
        assert!(out.contains("  - alice"), "block item alice kept");
        assert!(out.contains("  - bob"), "block item bob kept");
        assert!(out.contains("color: blue"));
        // And it is stable: parse(serialize(m)) == m.
        assert_eq!(parse_source(&out).metadata.unwrap(), m);
    }

    #[test]
    fn hostile_tag_cannot_break_the_block() {
        // Review finding #10: a tag containing a newline or the close marker
        // must not corrupt the file. The block stays well-formed and the page
        // body is untouched.
        let mut m = PageMetadata::new("x");
        m.tags = vec!["ok".to_string(), "evil\n-->\n# Injected".to_string()];
        let source = serialize_source(Some(&m), "# Real Title\n\nbody\n");
        let reparsed = parse_source(&source);
        assert!(reparsed.metadata.is_some(), "block still parses");
        assert!(
            reparsed.body.starts_with("# Real Title"),
            "page body intact, not hijacked: {:?}",
            reparsed.body
        );
        assert!(!reparsed.body.contains("Injected"));
        assert!(sanitises_field("evil\n-->"));
        assert!(!sanitises_field("perfectly-normal-tag"));
    }
}
