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
    //
    // The engine writes every git argument as a separately-quoted array element
    // (`&["merge", "--ff-only", "@{u}"]`), so a destructive token would appear
    // in the source quoted, e.g. `"checkout"`. Bare needles are used where the
    // word cannot occur benignly in prose; the two that CAN (`clean` collides
    // with `is_clean`, `-f` with `--ff-only`) use the quoted form so they match
    // a real argument without a false positive. An earlier version used
    // `"checkout -"` / `"clean -"`, which — with a space and dash — could never
    // match the array style and so silently proved nothing.
    let forbidden = [
        // Remote history overwriting.
        "--force",          // push/fetch --force (also matches --force-with-lease)
        "force-with-lease", // still replaces the remote tip
        "\"-f\"",           // short force flag, quoted so as not to hit "--ff-only"
        "+refs",            // force refspec
        "+head",            // force refspec targeting HEAD
        "--mirror",         // push --mirror can delete remote refs
        "--delete",         // push --delete removes a remote ref
        // Local history / working-tree discarding.
        "--hard",           // reset --hard
        "reset",            // any reset; the engine uses none
        "restore",          // discards uncommitted changes to tracked files
        "checkout",         // checkout -- <path> / -f discards changes; unused
        "\"clean\"",        // clean -fd deletes untracked (quoted: not `is_clean`)
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
