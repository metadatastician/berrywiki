// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Naming an imported page so that importing twice is importing once.
//!
//! `berrywiki-serve` mints page ids at random (UUIDv7, seeded from the clock)
//! because a person creating a page interactively wants a fresh identity every
//! time. An importer wants the opposite. Running the same notebook through
//! twice should produce the same tree, byte for byte, and it can only do that
//! if the id of a page is a function of the thing it was made from.
//!
//! So an imported id is derived: SHA-256 over a marker string, cut down to a
//! UUIDv8. Version 8 is the one the specification reserves for
//! implementation-defined layouts, which is exactly what a name-derived id is,
//! and it keeps the id visibly distinct from the v7 ids `serve` mints.
//!
//! The same marker is written into the page's metadata under the `source:`
//! key. Core preserves unknown keys verbatim (see
//! `berrywiki_core::metadata`), so the marker survives a round trip through
//! BerryWiki, through a plain clone, and through GitHub's own reader, none of
//! which know what it means.
//!
//! **The honest limit.** The marker carries the hash of the whole source file,
//! so editing the notebook and re-importing produces a new set of pages beside
//! the old ones rather than updating them. This is an importer, not a
//! synchroniser, and the difference is deliberate: guessing which edited node
//! corresponds to which existing page is the kind of guess that loses work.

use crate::hash::sha256;

/// Build the marker recorded under `source:` for one node.
///
/// `file_hash` is the hex SHA-256 of the source file exactly as read;
/// `node_id` is CherryTree's own `unique_id` for the node.
pub fn cherrytree_marker(file_hash: &str, node_id: &str) -> String {
    format!("cherrytree:{file_hash}:{node_id}")
}

/// Split a marker back into its source hash and node id.
///
/// Returns `None` for a marker this crate did not write, including one from a
/// future format, so an unrecognised `source:` line is left alone rather than
/// half-understood.
pub fn parse_cherrytree_marker(marker: &str) -> Option<(&str, &str)> {
    let rest = marker.strip_prefix("cherrytree:")?;
    let (file_hash, node_id) = rest.split_once(':')?;
    if file_hash.is_empty() || node_id.is_empty() {
        return None;
    }
    Some((file_hash, node_id))
}

/// Derive the page id for a marker.
///
/// The domain-separating prefix means a marker string that happens to be
/// hashed for some other purpose elsewhere cannot collide with a page id.
pub fn page_id(marker: &str) -> String {
    let digest = sha256(format!("berrywiki-import:{marker}").as_bytes());
    uuid_v8(&digest)
}

/// Lay the first sixteen bytes of a digest out as a UUIDv8.
fn uuid_v8(digest: &[u8; 32]) -> String {
    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    // Version 8: implementation-defined. Variant 10x: RFC 4122.
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;

    let hex = |bytes: &[u8]| -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            s.push(char::from_digit((byte >> 4) as u32, 16).expect("nibble is < 16"));
            s.push(char::from_digit((byte & 0x0f) as u32, 16).expect("nibble is < 16"));
        }
        s
    };

    format!(
        "{}-{}-{}-{}-{}",
        hex(&b[0..4]),
        hex(&b[4..6]),
        hex(&b[6..8]),
        hex(&b[8..10]),
        hex(&b[10..16]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_round_trips() {
        let m = cherrytree_marker("abc123", "7");
        assert_eq!(m, "cherrytree:abc123:7");
        assert_eq!(parse_cherrytree_marker(&m), Some(("abc123", "7")));
    }

    #[test]
    fn a_foreign_marker_is_not_half_understood() {
        assert_eq!(parse_cherrytree_marker("zim:abc:7"), None);
        assert_eq!(parse_cherrytree_marker("cherrytree:abc"), None);
        assert_eq!(parse_cherrytree_marker("cherrytree::7"), None);
        assert_eq!(parse_cherrytree_marker("cherrytree:abc:"), None);
    }

    #[test]
    fn the_same_marker_always_gives_the_same_id() {
        let a = page_id("cherrytree:deadbeef:3");
        let b = page_id("cherrytree:deadbeef:3");
        assert_eq!(a, b);
    }

    #[test]
    fn different_nodes_get_different_ids() {
        let a = page_id("cherrytree:deadbeef:3");
        let b = page_id("cherrytree:deadbeef:4");
        assert_ne!(a, b);
    }

    #[test]
    fn the_id_is_uuid_shaped_and_version_eight() {
        let id = page_id("cherrytree:deadbeef:3");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
        // Version nibble and variant bits, the two fields that say what kind
        // of UUID this is.
        assert!(parts[2].starts_with('8'), "version nibble: {id}");
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "variant nibble: {id}"
        );
    }

    #[test]
    fn the_id_is_a_legal_page_id() {
        // Mirrors berrywiki_store::paths::validate_page_id without depending
        // on it: non-empty, at most 128 bytes, no leading dot, and only
        // ASCII alphanumerics plus `-`, `_` and `.`.
        let id = page_id("cherrytree:deadbeef:3");
        assert!(!id.is_empty());
        assert!(id.len() <= 128);
        assert!(!id.starts_with('.'));
        assert!(id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')));
    }
}
