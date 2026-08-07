//! UUIDv7-shaped page-id minting from std only.
//!
//! The store leaves id generation to the application layer
//! (`berrywiki-store/src/lib.rs`, `CreatePageInput`). This crate is
//! deliberately free of third-party dependencies, so the id is assembled by
//! hand: a 48-bit unix-millisecond timestamp (v7 layout, so ids sort roughly
//! by creation time) plus entropy from `RandomState` (per-call random seed),
//! the pid and a process-local counter. That guarantees **uniqueness in
//! practice, not cryptographic unpredictability** — page ids are public
//! identifiers, never secrets.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn new_page_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let mut h = RandomState::new().build_hasher();
    h.write_u64(ms);
    h.write_u32(std::process::id());
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    let r1 = h.finish();
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(r1);
    let r2 = h2.finish();

    // Layout: 48-bit time | ver 7 | 12 bits random | variant 10 | 62 bits random.
    format!(
        "{:08x}-{:04x}-7{:03x}-{:04x}-{:012x}",
        (ms >> 16) & 0xffff_ffff,
        ms & 0xffff,
        r1 & 0xfff,
        0x8000 | ((r2 >> 48) & 0x3fff) as u16,
        r2 & 0xffff_ffff_ffff,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_is_uuid_like_and_store_valid() {
        let id = new_page_id();
        let groups: Vec<&str> = id.split('-').collect();
        assert_eq!(groups.len(), 5, "{id}");
        assert_eq!(
            groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12],
            "{id}"
        );
        assert!(groups[2].starts_with('7'), "version nibble: {id}");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        berrywiki_store::paths::validate_page_id(&id)
            .expect("minted id must pass store validation");
    }

    #[test]
    fn ids_are_unique_across_calls() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(new_page_id()), "duplicate id minted");
        }
    }
}
