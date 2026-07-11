//! Behavioural tests for the store⇄git wiring, run against a real bare remote
//! plus two clones (`berrywiki_git_compat::GitSandbox`). Every mutating test
//! also asserts nothing was lost and the remote was never force-updated.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;

use berrywiki_core::PageKind;
use berrywiki_git::{GitRepo, Identity};
use berrywiki_git_compat::GitSandbox;
use berrywiki_store::{CreatePageInput, LocalFolderStore, MovePageInput};
use berrywiki_sync::{SyncOutcome, SyncedStore, SyncError};

// App-state (journal) is keyed by the wiki's canonical path, so a single temp
// XDG_STATE_HOME shared across tests stays isolated per sandbox. Set once,
// before any store opens, so threaded tests never race on the env var.
static XDG: Once = Once::new();
fn init_xdg() {
    XDG.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bw-sync-xdg-{}", std::process::id()));
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

fn identity() -> Identity {
    Identity {
        name: "BerryWiki Test".to_string(),
        email: "test@berrywiki.invalid".to_string(),
    }
}

fn synced(clone: &Path) -> SyncedStore<LocalFolderStore> {
    init_xdg();
    SyncedStore::open_local(clone, identity()).expect("open synced wiki")
}

fn page(title: &str, id: &str, parent: Option<&str>) -> CreatePageInput {
    CreatePageInput {
        id: id.to_string(),
        title: title.to_string(),
        parent_id: parent.map(str::to_string),
        position: 0,
        kind: PageKind::Page,
        tags: Vec::new(),
        body: String::new(),
    }
}

fn commit_count(sb: &GitSandbox, clone: &Path) -> usize {
    sb.git(clone, &["rev-list", "--count", "HEAD"])
        .expect_success("rev-list")
        .stdout
        .trim()
        .parse()
        .unwrap()
}

fn files_in(sb: &GitSandbox, clone: &Path, rev: &str) -> Vec<String> {
    // --no-renames so a rename shows as its raw delete(old)+add(new), letting a
    // test see every path a commit actually changed (git's default rename
    // detection would collapse the pair to just the new name).
    sb.git(clone, &["show", "--name-only", "--no-renames", "--format=", rev])
        .expect_success("show")
        .stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn subject(sb: &GitSandbox, clone: &Path, rev: &str) -> String {
    sb.git(clone, &["log", "-1", "--format=%s", rev])
        .expect_success("log")
        .stdout
        .trim()
        .to_string()
}

// ---------- commit-on-save: one mutation == one atomic logical commit ----------

#[test]
fn create_is_one_commit_carrying_the_sidebar() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    let before = commit_count(&sb, &sb.ours);

    let saved = w.create_page(page("Notes", "notes-1", None)).unwrap();
    assert!(saved.checkpoint.is_none(), "clean tree needs no checkpoint");
    assert!(saved.commit.is_some());
    assert_eq!(commit_count(&sb, &sb.ours), before + 1, "exactly one commit");

    let files = files_in(&sb, &sb.ours, "HEAD");
    assert!(files.iter().any(|f| f == "Notes.md"), "page file: {files:?}");
    assert!(files.iter().any(|f| f == "_Sidebar.md"), "sidebar same commit: {files:?}");
    assert!(w.status().unwrap().is_clean());
}

#[test]
fn byte_identical_update_makes_no_commit() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    w.create_page(page("Stable", "stable-1", None)).unwrap();
    let body = w.read_page("stable-1").unwrap().body.clone();
    let before = commit_count(&sb, &sb.ours);

    let saved = w.update_page("stable-1", &body).unwrap();
    assert!(saved.commit.is_none(), "no commit for a no-op write");
    assert_eq!(commit_count(&sb, &sb.ours), before, "HEAD unchanged");
}

#[test]
fn nonleaf_move_is_a_single_atomic_commit() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    w.create_page(page("GrandParent", "gp-1", None)).unwrap();
    w.create_page(page("Parent", "p-1", None)).unwrap();
    w.create_page(page("Child", "c-1", Some("p-1"))).unwrap();
    let before = commit_count(&sb, &sb.ours);

    let saved = w
        .move_page(MovePageInput {
            id: "p-1".to_string(),
            new_parent_id: Some("gp-1".to_string()),
            new_position: 0,
        })
        .unwrap();
    assert!(saved.commit.is_some());
    assert_eq!(commit_count(&sb, &sb.ours), before + 1, "the whole cascade is one commit");

    // The root contributes no filename segment (ADR-0001), so `Child` was
    // `Child.md` while `Parent` was a root; moving `Parent` under `GrandParent`
    // makes `Parent` non-root, so the descendant gains the prefix. Both the old
    // and the new descendant name, plus the regenerated sidebar, are in the ONE
    // move commit.
    let files = files_in(&sb, &sb.ours, "HEAD");
    assert!(files.iter().any(|f| f == "Parent--Child.md"), "descendant's new prefixed name: {files:?}");
    assert!(files.iter().any(|f| f == "Child.md"), "descendant's old name (deleted): {files:?}");
    assert!(files.iter().any(|f| f == "_Sidebar.md"), "sidebar in the same commit: {files:?}");
}

#[test]
fn delete_commits_with_the_page_title_captured_before_removal() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    w.create_page(page("Ephemeral", "e-1", None)).unwrap();

    let saved = w.delete_page("e-1").unwrap();
    assert!(saved.commit.is_some());
    assert_eq!(subject(&sb, &sb.ours, "HEAD"), "Delete page \"Ephemeral\"");
}

#[test]
fn attachment_commit_does_not_touch_the_sidebar() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    w.create_page(page("Doc", "d-1", None)).unwrap();

    let saved = w.add_attachment("d-1", "diagram.png", b"\x89PNG\r\n\x1a\n").unwrap();
    assert!(saved.commit.is_some());
    assert_eq!(saved.value.filename, "diagram.png");

    let files = files_in(&sb, &sb.ours, "HEAD");
    assert!(files.iter().any(|f| f.contains("diagram.png")), "asset committed: {files:?}");
    assert!(!files.iter().any(|f| f == "_Sidebar.md"), "no sidebar change: {files:?}");
}

// ---------- clean-tree precondition / no data loss ----------

#[test]
fn external_edit_is_checkpointed_as_its_own_commit() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    // A plain-git edit lands in the tree before BerryWiki saves.
    fs::write(sb.ours.join("Loose-Note.md"), "# Loose\n\noutside berrywiki\n").unwrap();

    let saved = w.create_page(page("Fresh", "f-1", None)).unwrap();
    assert!(saved.checkpoint.is_some(), "the external edit was checkpointed first");

    assert_eq!(subject(&sb, &sb.ours, "HEAD"), "Create page \"Fresh\"");
    assert_eq!(subject(&sb, &sb.ours, "HEAD~1"), "Record changes made outside BerryWiki");
    assert!(files_in(&sb, &sb.ours, "HEAD~1").iter().any(|f| f == "Loose-Note.md"));
    assert!(!files_in(&sb, &sb.ours, "HEAD").iter().any(|f| f == "Loose-Note.md"), "not folded into the create commit");
}

#[test]
fn external_edit_to_the_same_page_is_checkpointed_not_clobbered() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    w.create_page(page("Target", "t-1", None)).unwrap(); // fingerprint captured

    // Foreign-edit the page directly on disk (bypassing the store).
    let path = sb.ours.join("Target.md");
    let original = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{original}\nforeign addition\n")).unwrap();

    let err = w.update_page("t-1", "# Target\n\nmy replacement\n").unwrap_err();
    assert!(matches!(err, SyncError::Store(_)), "the foreign edit trips StaleWrite: {err}");

    // The foreign edit is safe in history and still on disk — never clobbered.
    assert_eq!(subject(&sb, &sb.ours, "HEAD"), "Record changes made outside BerryWiki");
    assert!(fs::read_to_string(&path).unwrap().contains("foreign addition"));
}

#[test]
fn an_in_progress_merge_is_refused() {
    let sb = GitSandbox::create(&fixture_dir());
    // Create a genuine same-page conflict and leave the merge unresolved.
    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("push");
    sb.commit_change(&sb.ours, "Research.md", "# Research\n\nours\n", "Ours");
    sb.git(&sb.ours, &["fetch", "origin"]).expect_success("fetch");
    assert!(!sb.git(&sb.ours, &["merge", "--no-edit", "origin/main"]).success);

    let mut w = synced(&sb.ours);
    let err = w.create_page(page("X", "x-1", None)).unwrap_err();
    assert!(matches!(err, SyncError::UnmergedPaths { .. }), "unmerged tree refused: {err}");
}

#[test]
fn a_detached_head_is_refused_even_when_clean() {
    let sb = GitSandbox::create(&fixture_dir());
    let head = sb.head(&sb.ours);
    sb.git(&sb.ours, &["checkout", &head]).expect_success("detach"); // clean, detached
    let mut w = synced(&sb.ours);

    let err = w.create_page(page("Y", "y-1", None)).unwrap_err();
    assert!(matches!(err, SyncError::DetachedHead), "detached HEAD refused: {err}");
}

// ---------- the sync cycle (non-diverged) ----------

#[test]
fn sync_reports_up_to_date_when_nothing_changed() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    let report = w.sync().unwrap();
    assert_eq!(report.outcome, SyncOutcome::UpToDate);
    assert!(report.checkpoint.is_none());
}

#[test]
fn sync_reports_no_remote_but_still_commits_locally() {
    let sb = GitSandbox::create(&fixture_dir());
    sb.git(&sb.ours, &["branch", "--unset-upstream"]).expect_success("unset upstream");
    let mut w = synced(&sb.ours);

    let saved = w.create_page(page("Local", "l-1", None)).unwrap();
    assert!(saved.commit.is_some(), "commit-on-save works offline");
    assert_eq!(w.sync().unwrap().outcome, SyncOutcome::NoRemote);
    assert_eq!(subject(&sb, &sb.ours, "HEAD"), "Create page \"Local\"", "local commit survives");
}

#[test]
fn sync_integrates_a_remote_only_change_by_fast_forward() {
    let sb = GitSandbox::create(&fixture_dir());
    sb.commit_change(&sb.theirs, "New-Remote-Page.md", "# New Remote Page\n\nhi\n", "Add remote page");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("push");

    let mut w = synced(&sb.ours);
    assert_eq!(w.sync().unwrap().outcome, SyncOutcome::Integrated { fetched: 1 });
    assert!(sb.ours.join("New-Remote-Page.md").exists(), "remote file now local");
    assert!(w.list_pages().iter().any(|p| p.path == "New-Remote-Page.md"), "graph reloaded");
    assert!(w.status().unwrap().is_clean());
}

#[test]
fn sync_publishes_local_commits() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    w.create_page(page("Publish Me", "pub-1", None)).unwrap();

    assert_eq!(w.sync().unwrap().outcome, SyncOutcome::Published { pushed: 1 });
    // The other clone can now see the published commit (fetch-before-push held).
    assert_eq!(sb.behind_by(&sb.theirs), 1, "remote advanced");
}

#[test]
fn two_parties_round_trip_through_the_remote() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut a = synced(&sb.ours);
    a.create_page(page("Shared", "s-1", None)).unwrap();
    assert_eq!(a.sync().unwrap().outcome, SyncOutcome::Published { pushed: 1 });

    let mut b = synced(&sb.theirs);
    assert_eq!(b.sync().unwrap().outcome, SyncOutcome::Integrated { fetched: 1 });
    assert!(sb.theirs.join("Shared.md").exists());
}

// ---------- divergence hand-off / never force / no auto-merge (keystone) ----------

#[test]
fn diverged_history_is_handed_off_without_any_merge() {
    let sb = GitSandbox::create(&fixture_dir());
    // They publish an edit to one page.
    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs edit");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("push");
    let their_tip = sb.head(&sb.theirs);
    let remote_before = sb.head(&sb.remote);

    // We commit a different change locally, then sync.
    let mut w = synced(&sb.ours);
    w.create_page(page("Ours Only", "o-1", None)).unwrap();
    let our_tip = sb.head(&sb.ours);

    let report = w.sync().unwrap();
    let handoff = match report.outcome {
        SyncOutcome::Diverged(h) => h,
        other => panic!("expected Diverged, got {other:?}"),
    };
    assert_eq!(handoff.ahead, 1);
    assert_eq!(handoff.behind, 1);
    assert_eq!(handoff.local.0, our_tip, "handoff.local is our HEAD");
    assert_eq!(handoff.upstream.0, their_tip, "handoff.upstream is the fetched remote tip");

    // Hand-off postconditions: pristine, no merge started, remote untouched.
    assert!(w.status().unwrap().is_clean(), "no half-merge in the tree");
    assert!(!sb.ours.join(".git/MERGE_HEAD").exists(), "no merge in progress");
    assert_eq!(sb.head(&sb.ours), our_tip, "HEAD untouched");
    assert_eq!(sb.head(&sb.remote), remote_before, "remote not force-updated");
    assert_eq!(sb.head(&sb.remote), their_tip);
}

// ---------- push race (deterministic via the split API) ----------

#[test]
fn a_push_race_maps_to_pushraced_then_diverged_on_resync() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut w = synced(&sb.ours);
    w.create_page(page("Racer", "r-1", None)).unwrap(); // ahead 1

    // Fetch while the remote is still old; we are cleanly publishable.
    let refreshed = w.refresh().unwrap();
    assert!(refreshed.divergence.can_publish());

    // Now a third party advances the remote — our tracking ref is stale.
    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("push");
    let remote_tip = sb.head(&sb.remote);

    // advance() acts on the stale snapshot (no re-fetch): the push is rejected.
    assert_eq!(w.advance().unwrap(), SyncOutcome::PushRaced);
    assert_eq!(sb.head(&sb.remote), remote_tip, "remote not force-updated by the rejected push");

    // A full sync re-fetches and reclassifies to a genuine divergence.
    assert!(matches!(w.sync().unwrap().outcome, SyncOutcome::Diverged(_)));
}
