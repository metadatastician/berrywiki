// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! INV-1: metadata serialisation is idempotent. Whatever the input, one
//! parse-then-serialise pass reaches a fixed point that a second pass does not
//! move, and parsing never panics.
#![no_main]

use berrywiki_core::metadata::{parse_source, serialize_source};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|src: &str| {
    let first = parse_source(src);
    let once = serialize_source(first.metadata.as_ref(), &first.body);
    let second = parse_source(&once);
    let twice = serialize_source(second.metadata.as_ref(), &second.body);
    assert_eq!(once, twice, "serialisation is not idempotent");
    assert_eq!(
        first.metadata, second.metadata,
        "metadata changed on re-parse"
    );
});
