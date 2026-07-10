//! Filename generation and validation.
//!
//! Two distinct jobs:
//! 1. Turn page titles into flat `--`-separated filenames (ADR-0001).
//! 2. Reject any name that could escape the store root or break on the
//!    filesystems the wiki will be cloned to (Linux, macOS, **Windows**).

use crate::error::StoreError;
use crate::Result;

/// Characters forbidden in any single filename component. The union of
/// Windows-forbidden characters and path separators; NUL and control
/// characters are rejected separately.
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Windows reserved device names (case-insensitive, with or without extension).
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validate a single filename component (an attachment name or a generated
/// page filename). Rejects traversal, separators, Windows-hostile names,
/// control characters, and hidden/empty names.
pub fn validate_component(name: &str) -> Result<()> {
    let reject = |reason: &str| {
        Err(StoreError::InvalidName {
            name: name.to_string(),
            reason: reason.to_string(),
        })
    };

    if name.is_empty() {
        return reject("empty name");
    }
    if name.len() > 200 {
        return reject("longer than 200 bytes");
    }
    if name == "." || name == ".." {
        return reject("path traversal component");
    }
    if name.starts_with('.') {
        return reject("hidden files are not managed");
    }
    if name.chars().any(|c| c.is_control()) {
        return reject("control characters");
    }
    if let Some(bad) = name.chars().find(|c| FORBIDDEN.contains(c)) {
        return reject(&format!("forbidden character {bad:?}"));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return reject("trailing dot or space breaks Windows checkouts");
    }
    let stem = name.split('.').next().unwrap_or(name);
    if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        return reject("reserved Windows device name");
    }
    Ok(())
}

/// Slugify one title into one filename *segment*, preserving case.
///
/// Whitespace becomes `-`; alphanumerics (any script), `-` and `_` are kept;
/// everything else is dropped. Runs of `-` collapse to a single `-` so a title
/// can never inject the `--` hierarchy separator.
pub fn file_slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
        } else if c.is_whitespace() || c == '-' {
            out.push('-');
        }
        // everything else dropped
    }
    // Collapse '-' runs and trim.
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_dash = false;
    for c in out.chars() {
        if c == '-' {
            if !prev_dash {
                collapsed.push('-');
            }
            prev_dash = true;
        } else {
            collapsed.push(c);
            prev_dash = false;
        }
    }
    collapsed.trim_matches('-').to_string()
}

/// Build a flat page filename from ancestor titles + own title (ADR-0001),
/// e.g. `["Teaching", "Course A"], "Assessment Plan"` →
/// `Teaching--Course-A--Assessment-Plan.md`.
pub fn page_filename(ancestor_titles: &[String], title: &str) -> Result<String> {
    let mut segments: Vec<String> = Vec::with_capacity(ancestor_titles.len() + 1);
    for t in ancestor_titles {
        let s = file_slug(t);
        if !s.is_empty() {
            segments.push(s);
        }
    }
    let own = file_slug(title);
    if own.is_empty() {
        return Err(StoreError::InvalidName {
            name: title.to_string(),
            reason: "title produces an empty filename".to_string(),
        });
    }
    segments.push(own);
    let name = format!("{}.md", segments.join("--"));
    validate_component(&name)?;
    Ok(name)
}

/// Append a short id suffix for collision resolution
/// (`Assessment-Plan--e256.md`, ADR-0001).
pub fn with_id_suffix(filename: &str, id: &str) -> String {
    let stem = filename.strip_suffix(".md").unwrap_or(filename);
    let tail: String = id.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{stem}--{tail}.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_preserves_case_and_unicode() {
        assert_eq!(file_slug("Assessment Plan"), "Assessment-Plan");
        assert_eq!(file_slug("Sandbox — Ünïcode & Spaces"), "Sandbox-Ünïcode-Spaces");
    }

    #[test]
    fn slug_cannot_inject_separator() {
        // literal double hyphen and punctuation runs collapse to single '-'
        assert_eq!(file_slug("a--b"), "a-b");
        assert_eq!(file_slug("a - - b"), "a-b");
    }

    #[test]
    fn builds_hierarchical_filename() {
        let name =
            page_filename(&["Teaching".to_string(), "Course A".to_string()], "Assessment Plan")
                .unwrap();
        assert_eq!(name, "Teaching--Course-A--Assessment-Plan.md");
    }

    #[test]
    fn rejects_traversal_and_separators() {
        assert!(validate_component("../evil").is_err());
        assert!(validate_component("a/b").is_err());
        assert!(validate_component("a\\b").is_err());
        assert!(validate_component("..").is_err());
        assert!(validate_component(".hidden").is_err());
    }

    #[test]
    fn rejects_windows_hostile_names() {
        assert!(validate_component("CON.md").is_err());
        assert!(validate_component("nul").is_err());
        assert!(validate_component("trailing.").is_err());
        assert!(validate_component("trailing ").is_err());
        assert!(validate_component("que?stion.md").is_err());
    }

    #[test]
    fn id_suffix_is_short_and_stable() {
        assert_eq!(with_id_suffix("Plan.md", "0195f6ec-e256"), "Plan--e256.md");
    }
}
