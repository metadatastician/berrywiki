// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! The request-head parser never panics, and anything it accepts respects the
//! line limit it enforces (8 KiB).
#![no_main]

use berrywiki_serve::fuzz_entry::request_head;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some((method, target)) = request_head(data) {
        assert!(
            method.len() + target.len() <= 8 * 1024 + 1,
            "head line over the limit was accepted"
        );
    }
});
