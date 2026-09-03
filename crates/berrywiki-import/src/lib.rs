// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Reading a foreign notebook into BerryWiki.
//!
//! This crate is pure. It opens no file, writes no file, and holds no store:
//! it takes the bytes of a notebook and returns an [`ImportModel`], which the
//! CLI either prints (a dry run) or applies. A dry run and a real import
//! therefore compute exactly the same value, which is what makes the dry run
//! worth reading.
//!
//! # What it reads, and what it refuses by name
//!
//! CherryTree writes four formats, and only one of them is XML:
//!
//! | Extension | What it is | Here |
//! |---|---|---|
//! | `.ctd` | XML | read |
//! | `.ctb` | SQLite | refused by name |
//! | `.ctz` | 7-zip archive of a `.ctd` | refused by name |
//! | `.ctx` | 7-zip archive of a `.ctb` | refused by name |
//!
//! The three refusals are deliberate and are not a shrug. Reading `.ctb`
//! means a SQLite reader, and the workspace has exactly one third-party
//! dependency; the Solo contract's `cargo tree` assertion fails on a second.
//! Whether to take that dependency is an owner decision (D-11), recorded in
//! ADR-0014. Until it is ruled, a `.ctb` gets a message naming the format and
//! saying that CherryTree's own *File → Save As* converts it to `.ctd` in one
//! step, which is a better answer than a half-working reader.
//!
//! The refusal is checked against the *content* as well as the name, because
//! a notebook renamed to `.ctd` is still SQLite, and "malformed XML at byte
//! 0" is a worse diagnostic than "this is a SQLite notebook".

pub mod b64;
pub mod ctd;
pub mod hash;
pub mod markdown;
pub mod marker;
pub mod model;
pub mod report;
pub mod xml;

pub use ctd::parse_ctd;
pub use hash::{sha256, sha256_hex};
pub use marker::{cherrytree_marker, page_id, parse_cherrytree_marker};
pub use model::{ImportAsset, ImportModel, ImportNode};
pub use report::{Mode, Run};

/// A notebook format, as identified from a filename and the leading bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// CherryTree XML. The one format this crate reads.
    Ctd,
    /// CherryTree SQLite.
    Ctb,
    /// A 7-zip archive holding a `.ctd`.
    Ctz,
    /// A 7-zip archive holding a `.ctb`.
    Ctx,
    /// Neither the name nor the bytes said what this is.
    Unknown,
}

impl Format {
    /// The extension a file of this format normally carries.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Ctd => "ctd",
            Format::Ctb => "ctb",
            Format::Ctz => "ctz",
            Format::Ctx => "ctx",
            Format::Unknown => "",
        }
    }
}

const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";
const SEVENZIP_MAGIC: &[u8] = &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C];

/// Identify a notebook from its name and its first bytes.
///
/// Content wins over the name wherever the two disagree, because the name is
/// the part a user can change by accident.
pub fn detect(filename: &str, bytes: &[u8]) -> Format {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    if bytes.starts_with(SQLITE_MAGIC) {
        // A SQLite notebook is `.ctb` however it is named, unless it was
        // named `.ctx`, in which case the name is the better guess at intent.
        return if ext == "ctx" {
            Format::Ctx
        } else {
            Format::Ctb
        };
    }
    if bytes.starts_with(SEVENZIP_MAGIC) {
        return if ext == "ctz" {
            Format::Ctz
        } else {
            Format::Ctx
        };
    }

    match ext.as_str() {
        "ctd" => Format::Ctd,
        "ctb" => Format::Ctb,
        "ctz" => Format::Ctz,
        "ctx" => Format::Ctx,
        _ => {
            // No usable extension: fall back to looking for the root element.
            let head = &bytes[..bytes.len().min(4096)];
            if let Ok(text) = std::str::from_utf8(head) {
                if text.contains("<cherrytree") {
                    return Format::Ctd;
                }
            }
            Format::Unknown
        }
    }
}

/// Why a notebook was refused, in words meant for the person holding it.
pub fn refusal_message(format: Format) -> Option<String> {
    match format {
        Format::Ctd => None,
        Format::Ctb => Some(
            "This is a CherryTree SQLite notebook (.ctb). BerryWiki reads the XML format \
             (.ctd) only. In CherryTree, use File then Save As and choose the .ctd format, \
             then import that file. Nothing has been changed."
                .into(),
        ),
        Format::Ctz => Some(
            "This is a password-protected CherryTree notebook (.ctz): a 7-zip archive of a \
             .ctd. BerryWiki does not open archives and will not ask for your password. In \
             CherryTree, use File then Save As and choose the unprotected .ctd format, then \
             import that file. Nothing has been changed."
                .into(),
        ),
        Format::Ctx => Some(
            "This is a password-protected CherryTree notebook (.ctx): a 7-zip archive of a \
             .ctb. BerryWiki does not open archives and will not ask for your password. In \
             CherryTree, use File then Save As and choose the unprotected .ctd format, then \
             import that file. Nothing has been changed."
                .into(),
        ),
        Format::Unknown => Some(
            "This file is not a CherryTree notebook: it has no recognised extension and does \
             not begin with a <cherrytree> element. Nothing has been changed."
                .into(),
        ),
    }
}

/// Read a notebook's bytes into the import model.
///
/// `filename` is used for format detection only; no path is opened here.
/// A format this crate does not read is an `Err` carrying the message from
/// [`refusal_message`], never a partial model.
pub fn import(filename: &str, bytes: &[u8]) -> Result<ImportModel, String> {
    let format = detect(filename, bytes);
    if let Some(message) = refusal_message(format) {
        return Err(message);
    }

    let text = std::str::from_utf8(bytes).map_err(|e| {
        format!(
            "This .ctd file is not valid UTF-8 (first bad byte at offset {}). CherryTree \
             writes UTF-8, so the file has most likely been altered in transit. Nothing has \
             been changed.",
            e.valid_up_to()
        )
    })?;

    // The hash covers the file exactly as it arrived, before the BOM is
    // stripped, so re-importing the identical file always gives identical
    // page ids.
    let source_hash = sha256_hex(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    Ok(parse_ctd(text, &source_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ctd_is_read() {
        let src = br#"<cherrytree><node name="A" unique_id="1"><rich_text>hi</rich_text></node></cherrytree>"#;
        let m = import("notes.ctd", src).expect("a .ctd is readable");
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].title, "A");
        assert_eq!(m.source_hash, sha256_hex(src));
    }

    #[test]
    fn each_unread_format_is_refused_by_name() {
        for (name, bytes, needle) in [
            ("notes.ctb", &b""[..], "SQLite"),
            ("notes.ctz", &b""[..], "password-protected"),
            ("notes.ctx", &b""[..], "password-protected"),
        ] {
            let err = import(name, bytes).expect_err("this format is not read");
            assert!(err.contains(needle), "{name}: {err}");
            assert!(
                err.contains("Nothing has been changed"),
                "{name} must say nothing was changed: {err}"
            );
            assert!(err.contains(".ctd"), "{name} must name the way out: {err}");
        }
    }

    #[test]
    fn content_beats_the_name() {
        // A SQLite notebook renamed to .ctd is still SQLite, and saying so
        // is far more use than "malformed XML at byte 0".
        let mut bytes = b"SQLite format 3\0".to_vec();
        bytes.extend_from_slice(&[0u8; 16]);
        assert_eq!(detect("notes.ctd", &bytes), Format::Ctb);
        let err = import("notes.ctd", &bytes).expect_err("SQLite is not read");
        assert!(err.contains("SQLite"), "{err}");

        let seven = [0x37u8, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0, 0];
        assert_eq!(detect("notes.ctd", &seven), Format::Ctx);
    }

    #[test]
    fn a_ctd_without_an_extension_is_still_recognised() {
        let src =
            br#"<?xml version="1.0"?><cherrytree><node name="A" unique_id="1"/></cherrytree>"#;
        assert_eq!(detect("notebook", src), Format::Ctd);
    }

    #[test]
    fn something_else_entirely_is_refused_not_parsed() {
        let err = import("notes.txt", b"just some prose").expect_err("prose is not a notebook");
        assert!(err.contains("not a CherryTree notebook"), "{err}");
    }

    #[test]
    fn a_byte_order_mark_does_not_defeat_the_parser() {
        let src = "\u{feff}<cherrytree><node name=\"A\" unique_id=\"1\"/></cherrytree>";
        let m = import("notes.ctd", src.as_bytes()).expect("a BOM is tolerated");
        assert_eq!(m.nodes.len(), 1);
    }

    #[test]
    fn invalid_utf8_is_refused_with_the_offset() {
        let bad = [b'<', b'c', b'h', b'e', b'r', b'r', b'y', 0xff, 0xfe];
        let err = import("notes.ctd", &bad).expect_err("invalid UTF-8 is refused");
        assert!(err.contains("not valid UTF-8"), "{err}");
    }
}
