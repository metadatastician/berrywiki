// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! The hand-rolled XML reader always terminates: every event consumes input,
//! so the number of events is bounded by the input length.
#![no_main]

use berrywiki_import::xml::Reader;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|src: &str| {
    let mut reader = Reader::new(src);
    let bound = src.len() + 2;
    for _ in 0..bound {
        match reader.next_event() {
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return,
        }
    }
    panic!(
        "more than {bound} events from {} bytes: the reader is not consuming input",
        src.len()
    );
});
