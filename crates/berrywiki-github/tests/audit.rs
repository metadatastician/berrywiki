// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Structural audit of the mirror crate's destructive git use (v1 criterion S5).
//!
//! `berrywiki-git` may not express any destructive git operation at all (its
//! own `tests/audit.rs`). This crate is the managed *mirror*, and it needs
//! exactly one: the `reset --hard` that re-syncs a cache to its remote. The
//! audit pins that down: one hard reset, preceded in the source by the
//! ownership check that makes it safe, and nothing else destructive anywhere.
//!
//! Git arguments are written as quoted array elements (`&["reset", "--hard"]`),
//! so the needles are quoted forms; prose in comments cannot trip them.
//!
//! The audit itself is shown to fail: [`audit`] is also run against planted
//! sources that break each rule.

/// The mirror crate's source, embedded at compile time.
const MIRROR_SRC: &str = include_str!("../src/lib.rs");

/// Destructive git arguments that may never appear in the mirror source.
const FORBIDDEN: &[&str] = &[
    "\"--force\"",
    "force-with-lease",
    "\"-f\"",
    "\"--mirror\"",
    "\"--delete\"",
    "\"--abort\"",
    "\"restore\"",
    "\"checkout\"",
    "\"clean\"",
];

/// Check `src` against the mirror rules; `Err` names the first violation.
fn audit(src: &str) -> Result<(), String> {
    for needle in FORBIDDEN {
        if src.contains(needle) {
            return Err(format!("forbidden git argument {needle}"));
        }
    }
    let resets = src.matches("\"reset\"").count();
    let hards = src.matches("\"--hard\"").count();
    if resets != 1 || hards != 1 {
        return Err(format!(
            "expected exactly one \"reset\" \"--hard\", found {resets} reset / {hards} --hard"
        ));
    }
    let guard = src
        .find("ensure_owned_mirror(dest, remote)?")
        .ok_or("the ownership check call is missing")?;
    let reset = src.find("\"reset\"").expect("counted above");
    if guard > reset {
        return Err("the hard reset is not preceded by the ownership check".into());
    }
    Ok(())
}

#[test]
fn the_mirror_source_passes_the_audit() {
    audit(MIRROR_SRC).unwrap();
}

#[test]
fn the_audit_rejects_planted_violations() {
    let guarded = r#"ensure_owned_mirror(dest, remote)?; run_git(&["reset", "--hard", "@{u}"]);"#;
    assert!(audit(guarded).is_ok(), "control: a guarded reset passes");

    let unguarded = r#"run_git(&["reset", "--hard", "@{u}"]);"#;
    assert!(audit(unguarded).is_err(), "an unguarded reset must fail");

    let late_guard = r#"run_git(&["reset", "--hard"]); ensure_owned_mirror(dest, remote)?;"#;
    assert!(
        audit(late_guard).is_err(),
        "a guard after the reset must fail"
    );

    let second = format!("{guarded} run_git(&[\"reset\", \"--hard\", \"HEAD\"]);");
    assert!(audit(&second).is_err(), "a second hard reset must fail");

    let forced = format!("{guarded} run_git(&[\"push\", \"--force\"]);");
    assert!(audit(&forced).is_err(), "a force push must fail");
}
