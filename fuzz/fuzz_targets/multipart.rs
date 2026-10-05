// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! The multipart upload parser never panics, and a filename it accepts never
//! carries a path separator or NUL (the store rejects those too, but the
//! parser must not be the place a traversal starts).
#![no_main]

use berrywiki_serve::fuzz_entry::multipart;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: (&str, &[u8])| {
    let (content_type, body) = input;
    for ct in [content_type, "multipart/form-data; boundary=X"] {
        if let Some(name) = multipart(ct, body) {
            assert!(
                !name.contains(['/', '\\', '\0']),
                "accepted filename with a separator: {name:?}"
            );
        }
    }
});
