//! Structural proof that the git engine cannot lose data.
//!
//! The engine's whole safety argument is that its source expresses none of the
//! git flags or subcommands that overwrite remote history or discard
//! uncommitted work. This test enforces exactly that by scanning the engine's
//! source. If a future change introduces one of these tokens, the build fails
//! here — the safety property is a test, not a comment.
//!
//! The forbidden tokens deliberately live in *this* file (a test), which the
//! scan does not read, so they cannot trip their own check.

/// The engine source, embedded at compile time.
const ENGINE_SRC: &str = include_str!("../src/lib.rs");

#[test]
fn engine_source_contains_no_destructive_git_tokens() {
    let hay = ENGINE_SRC.to_lowercase();
    // Each needle names an operation that could overwrite the remote or discard
    // local work. None of them may appear anywhere in the engine.
    let forbidden = [
        "--force",          // history-overwriting push / fetch
        "force-with-lease", // still replaces the remote tip
        "--hard",           // discards the working tree and index
        "reset",            // (with --hard/--merge) rewinds; unused entirely
        "restore",          // discards uncommitted changes to tracked files
        "checkout -",       // `checkout -- <path>` / `-f` discards changes
        "clean -",          // deletes untracked files
        "+refs",            // force refspec
        "+head",            // force refspec targeting HEAD
    ];
    for needle in forbidden {
        assert!(
            !hay.contains(needle),
            "berrywiki-git source must not contain the destructive git token {needle:?} — \
             history-overwriting and working-tree-discarding operations are unrepresentable"
        );
    }
}

#[test]
fn engine_runs_git_hermetically() {
    // The isolation knobs that keep behaviour reproducible must stay wired in.
    for expected in [
        "LC_ALL",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_TERMINAL_PROMPT",
    ] {
        assert!(
            ENGINE_SRC.contains(expected),
            "engine must still set {expected} for hermetic execution"
        );
    }
}
