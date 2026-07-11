//! Structural guarantee: the sync layer never shells out to git itself. Every
//! git operation must flow through the audited `berrywiki_git::GitRepo`, so the
//! engine's "destructive operations are unrepresentable" property keeps bounding
//! all behaviour reachable from here.

const SRC: &str = include_str!("../src/lib.rs");

#[test]
fn sync_layer_runs_no_raw_git() {
    assert!(
        !SRC.contains("std::process"),
        "berrywiki-sync must not use std::process — all git goes through GitRepo"
    );
    assert!(
        !SRC.contains("Command"),
        "berrywiki-sync must not spawn a process directly"
    );
}
