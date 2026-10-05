// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Whole-notebook import never panics, whatever the bytes, under any of the
//! names the importer dispatches on.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for name in ["notes.ctd", "notes.ctb", "notes.ctz", "notes"] {
        let _ = berrywiki_import::import(name, data);
    }
});
