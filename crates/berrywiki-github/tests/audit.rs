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

/// The one hard reset the mirror may run, written exactly as in the source.
const RESET_CALL: &str = r#"run_git(&["-C", dest_str, "reset", "--hard", "@{u}"], token)?"#;
/// The ownership check that must run before it, in the same function.
const GUARD_CALL: &str = "ensure_owned_mirror(dest, remote)?";

/// The body of `fn name(`, from its opening brace to the matching close.
fn fn_body<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let start = src.find(&format!("fn {name}("))?;
    let open = start + src[start..].find('{')?;
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[open..=open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Check `src` against the mirror rules; `Err` names the first violation.
///
/// The reset is pinned to its command and its control flow: the exact
/// `reset --hard @{u}` call must appear once in the whole file, it must be
/// inside `clone_or_update`, and that same function must call the ownership
/// check before it. No other `"reset"` or `"--hard"` argument may exist, so
/// the two cannot be split across commands or moved to another function.
fn audit(src: &str) -> Result<(), String> {
    for needle in FORBIDDEN {
        if src.contains(needle) {
            return Err(format!("forbidden git argument {needle}"));
        }
    }
    if src.matches(RESET_CALL).count() != 1 {
        return Err("the guarded reset call must appear exactly once, verbatim".into());
    }
    for arg in ["\"reset\"", "\"--hard\""] {
        if src.matches(arg).count() != 1 {
            return Err(format!(
                "{arg} appears outside the one permitted reset call"
            ));
        }
    }
    let body = fn_body(src, "clone_or_update").ok_or("fn clone_or_update is missing")?;
    let reset = body
        .find(RESET_CALL)
        .ok_or("the hard reset is not inside clone_or_update")?;
    let guard = body
        .find(GUARD_CALL)
        .ok_or("clone_or_update does not call the ownership check")?;
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
    let ok = format!("fn clone_or_update(a: u8) {{ {GUARD_CALL}; {RESET_CALL}; }}");
    assert!(audit(&ok).is_ok(), "control: a guarded reset passes");

    let unguarded = format!("fn clone_or_update(a: u8) {{ {RESET_CALL}; }}");
    assert!(audit(&unguarded).is_err(), "an unguarded reset must fail");

    let late = format!("fn clone_or_update(a: u8) {{ {RESET_CALL}; {GUARD_CALL}; }}");
    assert!(audit(&late).is_err(), "a guard after the reset must fail");

    let elsewhere = format!(
        "fn clone_or_update(a: u8) {{ {GUARD_CALL}; }} fn other(b: u8) {{ {RESET_CALL}; }}"
    );
    assert!(
        audit(&elsewhere).is_err(),
        "a reset in another function must fail"
    );

    let split = format!(
        "fn clone_or_update(a: u8) {{ {GUARD_CALL}; run_git(&[\"reset\"]); run_git(&[\"--hard\"]); }}"
    );
    assert!(
        audit(&split).is_err(),
        "reset and --hard split across commands must fail"
    );

    let second =
        format!("fn clone_or_update(a: u8) {{ {GUARD_CALL}; {RESET_CALL}; {RESET_CALL}; }}");
    assert!(audit(&second).is_err(), "a second hard reset must fail");

    let forced = format!("{ok} run_git(&[\"push\", \"--force\"]);");
    assert!(audit(&forced).is_err(), "a force push must fail");
}
