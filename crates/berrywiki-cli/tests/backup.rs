// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! `berrywiki backup` / `berrywiki restore` against real git (ADR-0013).
//!
//! The round trip is the point: a backup that cannot be restored is not a
//! backup. Every refusal is tested too, because the refusals are what stop the
//! commands from destroying the thing they exist to protect.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;

use berrywiki_git_compat::GitSandbox;

// App state is keyed by a hash of the wiki's path, so one temp XDG_STATE_HOME
// shared across tests stays isolated per sandbox. Set once, before any store
// opens, so threaded tests never race on the env var.
static XDG: Once = Once::new();
fn init_xdg() {
    XDG.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bw-cli-backup-xdg-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("XDG_STATE_HOME", &dir);
    });
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-wiki")
        .canonicalize()
        .expect("fixture exists")
}

fn run(args: &[&str]) -> (i32, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut buf = Vec::new();
    let code = berrywiki_cli::run(&args, &mut buf).expect("cli ran");
    (code, String::from_utf8(buf).expect("utf-8 output"))
}

/// A scratch path that does not exist yet, for a command that must create its
/// own destination.
fn scratch(sb: &GitSandbox, name: &str) -> PathBuf {
    sb.root.join(name)
}

/// Every commit reachable from any ref, sorted, so two repositories can be
/// compared as object graphs rather than as one branch tip.
fn all_commits(sb: &GitSandbox, repo: &Path) -> Vec<String> {
    let mut ids: Vec<String> = sb
        .git(repo, &["rev-list", "--all"])
        .expect_success("rev-list --all")
        .stdout
        .lines()
        .map(str::to_string)
        .collect();
    ids.sort();
    ids
}

// ---------- the round trip ----------

#[test]
fn backup_then_restore_reproduces_history_and_drafts_at_the_new_path() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());

    // A commit that exists only here, so a restored copy that lacks it is
    // visibly wrong rather than plausibly stale.
    sb.commit_change(
        &sb.ours,
        "Research.md",
        "# Research\n\nonly in this clone\n",
        "Local-only research edit",
    );
    let source_head = sb.head(&sb.ours);
    let source_commits = all_commits(&sb, &sb.ours);
    let source_origin = sb
        .git(&sb.ours, &["remote", "get-url", "origin"])
        .expect_success("get-url")
        .stdout
        .trim()
        .to_string();

    // A draft is the state a clone does not carry, so it is the thing that
    // proves the backup covers more than git.
    let state = berrywiki_appstate::AppState::for_wiki(&sb.ours).expect("app state");
    fs::create_dir_all(state.drafts_dir()).expect("drafts dir");
    fs::write(state.drafts_dir().join("draft-one.md"), "unsaved thought\n").expect("write draft");

    let backup = scratch(&sb, "backup");
    let (code, out) = run(&[
        "backup",
        sb.ours.to_str().unwrap(),
        backup.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "backup should succeed:\n{out}");
    assert!(
        backup.join("wiki.bundle").is_file(),
        "bundle written:\n{out}"
    );
    assert!(
        backup.join("MANIFEST").is_file(),
        "manifest written:\n{out}"
    );
    assert!(
        backup.join("state/drafts/draft-one.md").is_file(),
        "draft archived:\n{out}"
    );

    let restored = scratch(&sb, "restored");
    let (code, out) = run(&[
        "restore",
        backup.to_str().unwrap(),
        restored.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "restore should succeed:\n{out}");

    assert_eq!(
        sb.head(&restored),
        source_head,
        "restored HEAD is the source HEAD"
    );
    assert_eq!(
        all_commits(&sb, &restored),
        source_commits,
        "every commit reachable in the source is reachable in the restored copy"
    );
    assert_eq!(
        fs::read_to_string(restored.join("Research.md")).expect("page present"),
        "# Research\n\nonly in this clone\n",
        "the local-only edit came back"
    );

    // The clone came from a bundle, so an unrepointed origin would name a file
    // that vanishes with the backup.
    let restored_origin = sb
        .git(&restored, &["remote", "get-url", "origin"])
        .expect_success("get-url")
        .stdout
        .trim()
        .to_string();
    assert_eq!(
        restored_origin, source_origin,
        "origin points at the recorded remote, not at the bundle file"
    );

    // The trap this test exists for: app state is keyed by a hash of the
    // wiki's path, so a draft written back to the *source's* state dir would be
    // invisible here even though the file exists somewhere.
    let restored_state =
        berrywiki_appstate::AppState::for_wiki(&restored).expect("restored app state");
    assert_ne!(
        restored_state.repo_id(),
        state.repo_id(),
        "a different path really does mean a different state dir"
    );
    assert_eq!(
        fs::read_to_string(restored_state.drafts_dir().join("draft-one.md"))
            .expect("draft visible through the restored wiki's own app state"),
        "unsaved thought\n"
    );
}

#[test]
fn a_backup_carries_no_lock_and_no_search_index() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let state = berrywiki_appstate::AppState::for_wiki(&sb.ours).expect("app state");
    fs::create_dir_all(state.index_dir()).expect("index dir");
    fs::write(state.index_dir().join("terms.bin"), "derived\n").expect("write index");

    let backup = scratch(&sb, "backup-no-derived");
    let (code, out) = run(&[
        "backup",
        sb.ours.to_str().unwrap(),
        backup.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{out}");

    assert!(
        !backup.join("state/index").exists(),
        "the index is derived data and is rebuilt, never archived"
    );
    assert!(
        !backup.join("state/lock").exists(),
        "the lock describes a running process, not the wiki"
    );
    assert!(
        out.contains("search index is not archived"),
        "the omission is stated, not silent:\n{out}"
    );
}

// ---------- refusals ----------

#[test]
fn backup_refuses_a_dirty_working_tree_and_names_the_files() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    fs::write(sb.ours.join("Research.md"), "# Research\n\nuncommitted\n").expect("dirty");

    let backup = scratch(&sb, "backup-dirty");
    let (code, out) = run(&[
        "backup",
        sb.ours.to_str().unwrap(),
        backup.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "a dirty tree is refused:\n{out}");
    assert!(out.contains("uncommitted changes"), "{out}");
    assert!(
        out.contains("Research.md"),
        "the pending path is named:\n{out}"
    );
    assert!(
        !backup.join("wiki.bundle").exists(),
        "nothing is written on refusal"
    );
}

#[test]
fn backup_refuses_a_destination_that_already_has_contents() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let backup = scratch(&sb, "backup-occupied");
    fs::create_dir_all(&backup).expect("mkdir");
    fs::write(backup.join("something-precious"), "keep me\n").expect("write");

    let (code, out) = run(&[
        "backup",
        sb.ours.to_str().unwrap(),
        backup.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("already has contents"), "{out}");
    assert_eq!(
        fs::read_to_string(backup.join("something-precious")).unwrap(),
        "keep me\n",
        "the existing file is untouched"
    );
}

#[test]
fn restore_refuses_a_target_that_already_has_contents() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let backup = scratch(&sb, "backup-for-occupied-restore");
    let (code, out) = run(&[
        "backup",
        sb.ours.to_str().unwrap(),
        backup.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{out}");

    let target = scratch(&sb, "occupied-target");
    fs::create_dir_all(&target).expect("mkdir");
    fs::write(target.join("my-work.md"), "not yours\n").expect("write");

    let (code, out) = run(&[
        "restore",
        backup.to_str().unwrap(),
        target.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("already has contents"), "{out}");
    assert_eq!(
        fs::read_to_string(target.join("my-work.md")).unwrap(),
        "not yours\n",
        "restore never writes over an existing clone"
    );
}

#[test]
fn restore_refuses_a_directory_that_is_not_a_backup() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());

    // No manifest at all.
    let empty = scratch(&sb, "not-a-backup");
    fs::create_dir_all(&empty).expect("mkdir");
    let (code, out) = run(&[
        "restore",
        empty.to_str().unwrap(),
        scratch(&sb, "t1").to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("does not look like a BerryWiki backup"),
        "{out}"
    );

    // A manifest, but not ours: the magic line is what is checked, so a file
    // that merely happens to be named MANIFEST is still refused.
    let wrong = scratch(&sb, "wrong-magic");
    fs::create_dir_all(&wrong).expect("mkdir");
    fs::write(wrong.join("MANIFEST"), "some-other-tool: 4\n").expect("write");
    let (code, out) = run(&[
        "restore",
        wrong.to_str().unwrap(),
        scratch(&sb, "t2").to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("refusing to guess at the format"), "{out}");
}

#[test]
fn both_commands_report_usage_when_given_one_path() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let (code, out) = run(&["backup", sb.ours.to_str().unwrap()]);
    assert_eq!(code, 2);
    assert!(out.contains("usage: berrywiki backup"), "{out}");

    let (code, out) = run(&["restore", sb.ours.to_str().unwrap()]);
    assert_eq!(code, 2);
    assert!(out.contains("usage: berrywiki restore"), "{out}");
}
