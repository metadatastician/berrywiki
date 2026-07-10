//! Phase 0 Step 6: reproduce the synchronisation situations BerryWiki must
//! survive, and prove that **local work is never lost** in any of them.
//!
//! Every assertion here is *evidence* for docs/compatibility/github-wiki.adoc
//! (the local-git rows; live GitHub behaviour remains separately gated).

use std::fs;
use std::path::PathBuf;

use berrywiki_git_compat::GitSandbox;

const PAGE: &str = "Teaching--Course-A--Assessment-Plan.md";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-wiki")
        .canonicalize()
        .expect("fixture exists")
}

#[test]
fn seed_commit_and_clone_round_trip() {
    let sb = GitSandbox::create(&fixture_dir());
    // Both clones see the fixture; the working trees are clean.
    assert!(sb.ours.join(PAGE).exists());
    assert!(sb.theirs.join(PAGE).exists());
    let status = sb.git(&sb.ours, &["status", "--porcelain"]).expect_success("status");
    assert!(status.stdout.trim().is_empty(), "clean tree after seed");
}

#[test]
fn remote_change_is_detected_before_push() {
    let sb = GitSandbox::create(&fixture_dir());
    // "They" edit and push while "we" do nothing.
    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\nremote edit\n", "Remote edit");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");

    // Fetch-before-push (non-negotiable rule 6) reveals we are behind.
    assert_eq!(sb.behind_by(&sb.ours), 1, "remote change visible after fetch");
}

#[test]
fn non_fast_forward_push_is_rejected_and_local_commit_survives() {
    let sb = GitSandbox::create(&fixture_dir());

    // Both sides commit different files; they push first.
    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");
    sb.commit_change(&sb.ours, "Teaching.md", "# Teaching\n\nours\n", "Ours");
    let our_head = sb.head(&sb.ours);

    // Our push must be rejected (no force), and our commit must be intact.
    let push = sb.git(&sb.ours, &["push", "origin", "main"]);
    assert!(!push.success, "non-fast-forward push must fail");
    assert!(
        push.stderr.contains("fetch first")
            || push.stderr.contains("non-fast-forward")
            || push.stderr.contains("rejected"),
        "rejection is explicit: {}",
        push.stderr
    );
    assert_eq!(sb.head(&sb.ours), our_head, "local commit untouched by rejection");
    let content = fs::read_to_string(sb.ours.join("Teaching.md")).unwrap();
    assert!(content.contains("ours"), "local content untouched");
}

#[test]
fn non_overlapping_changes_merge_cleanly() {
    let sb = GitSandbox::create(&fixture_dir());
    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");
    sb.commit_change(&sb.ours, "Teaching.md", "# Teaching\n\nours\n", "Ours");

    sb.git(&sb.ours, &["fetch", "origin"]).expect_success("fetch");
    let merge = sb.git(&sb.ours, &["merge", "--no-edit", "origin/main"]);
    assert!(merge.success, "disjoint files merge without conflict");

    // Both edits present; push now succeeds.
    assert!(fs::read_to_string(sb.ours.join("Research.md")).unwrap().contains("theirs"));
    assert!(fs::read_to_string(sb.ours.join("Teaching.md")).unwrap().contains("ours"));
    sb.git(&sb.ours, &["push", "origin", "main"]).expect_success("push after merge");
}

#[test]
fn same_page_conflict_preserves_both_versions_and_aborts_safely() {
    let sb = GitSandbox::create(&fixture_dir());

    sb.commit_change(&sb.theirs, PAGE, "# Assessment Plan\n\nTHEIR version\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");
    sb.commit_change(&sb.ours, PAGE, "# Assessment Plan\n\nOUR version\n", "Ours");
    let our_head = sb.head(&sb.ours);

    sb.git(&sb.ours, &["fetch", "origin"]).expect_success("fetch");
    let merge = sb.git(&sb.ours, &["merge", "--no-edit", "origin/main"]);
    assert!(!merge.success, "same-page edit must conflict");

    // The conflicted file carries BOTH versions — nothing is lost.
    let conflicted = fs::read_to_string(sb.ours.join(PAGE)).unwrap();
    assert!(conflicted.contains("OUR version"), "local side present");
    assert!(conflicted.contains("THEIR version"), "remote side present");
    assert!(conflicted.contains("<<<<<<<"), "standard conflict markers");

    // `git status` names the conflicted path (what a conflict UI will list).
    let status = sb.git(&sb.ours, &["status", "--porcelain"]).expect_success("status");
    assert!(status.stdout.lines().any(|l| l.starts_with("UU") && l.contains(PAGE)));

    // Aborting restores our version exactly; our commit is still HEAD.
    sb.git(&sb.ours, &["merge", "--abort"]).expect_success("abort");
    let restored = fs::read_to_string(sb.ours.join(PAGE)).unwrap();
    assert_eq!(restored, "# Assessment Plan\n\nOUR version\n");
    assert_eq!(sb.head(&sb.ours), our_head);
}

#[test]
fn sidebar_conflict_behaves_like_any_page_conflict() {
    let sb = GitSandbox::create(&fixture_dir());
    sb.commit_change(&sb.theirs, "_Sidebar.md", "# Notebook\n\n- [Theirs](Theirs)\n", "Their sidebar");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");
    sb.commit_change(&sb.ours, "_Sidebar.md", "# Notebook\n\n- [Ours](Ours)\n", "Our sidebar");

    sb.git(&sb.ours, &["fetch", "origin"]).expect_success("fetch");
    let merge = sb.git(&sb.ours, &["merge", "--no-edit", "origin/main"]);
    assert!(!merge.success, "generated-file conflict is still a conflict");
    let conflicted = fs::read_to_string(sb.ours.join("_Sidebar.md")).unwrap();
    assert!(conflicted.contains("Ours") && conflicted.contains("Theirs"));
    // Recovery strategy for generated files: abort, merge pages, regenerate.
    sb.git(&sb.ours, &["merge", "--abort"]).expect_success("abort");
}

#[test]
fn remote_delete_of_locally_edited_page_keeps_local_content() {
    let sb = GitSandbox::create(&fixture_dir());

    // They delete the page and push; we edit the same page.
    sb.git(&sb.theirs, &["rm", PAGE]).expect_success("their rm");
    sb.git(&sb.theirs, &["commit", "-m", "Delete page"]).expect_success("their commit");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");
    sb.commit_change(&sb.ours, PAGE, "# Assessment Plan\n\nedited locally\n", "Local edit");

    sb.git(&sb.ours, &["fetch", "origin"]).expect_success("fetch");
    let merge = sb.git(&sb.ours, &["merge", "--no-edit", "origin/main"]);
    assert!(!merge.success, "modify/delete must conflict, not silently delete");

    // git keeps the modified version in the working tree — content survives.
    let content = fs::read_to_string(sb.ours.join(PAGE)).unwrap();
    assert!(content.contains("edited locally"));

    let status = sb.git(&sb.ours, &["status", "--porcelain"]).expect_success("status");
    assert!(
        status.stdout.lines().any(|l| l.contains(PAGE)),
        "conflict surfaced in status: {}",
        status.stdout
    );
    sb.git(&sb.ours, &["merge", "--abort"]).expect_success("abort");
    assert!(sb.ours.join(PAGE).exists(), "local file still present after abort");
}
