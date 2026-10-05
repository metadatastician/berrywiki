// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! The no-script invariant: rendered HTML carries no way to run script,
//! whatever Markdown went in.
#![no_main]

use berrywiki_fuzz::html_violation;
use berrywiki_render::render_markdown;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|md: &str| {
    let html = render_markdown(md);
    if let Some(v) = html_violation(&html) {
        panic!("{v}\n--- markdown ---\n{md:?}\n--- html ---\n{html}");
    }
});
