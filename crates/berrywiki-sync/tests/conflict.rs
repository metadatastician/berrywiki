// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! P3-conflict: what BerryWiki says about a merge someone else started, and
//! the one kind of clash it will settle by itself.
//!
//! Every case below leaves a real unfinished merge in a real clone
//! (`berrywiki_git_compat::GitSandbox`) and reads the classification back out
//! of the git index, so the kinds are evidence rather than a guess. BerryWiki
//! never starts a merge, so the tests start it with plain git first.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;

use berrywiki_git::Identity;
use berrywiki_git_compat::GitSandbox;
use berrywiki_store::LocalFolderStore;
use berrywiki_sync::{ConflictKind, SyncError, SyncedStore};

const PLAN_FILE: &str = "Teaching--Course-A--Assessment-Plan.md";
const HOME_ID: &str = "0195f6d0-0000-7000-8000-000000000001";

static XDG: Once = Once::new();
fn init_xdg() {
    XDG.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bw-conflict-xdg-{}", std::process::id()));
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

fn sandbox() -> GitSandbox {
    init_xdg();
    GitSandbox::create(&fixture_dir())
}

fn synced(sb: &GitSandbox) -> SyncedStore<LocalFolderStore> {
    SyncedStore::open_local(&sb.ours, identity()).expect("open synced wiki")
}

/// A whole page, metadata block included, so a test can vary one part at a
/// time and know the other part is byte-identical on both sides.
fn page(id: &str, parent: &str, position: i64, title: &str, body: &str) -> String {
    format!(
        "<!-- berrywiki\nid: {id}\nparent: {parent}\nposition: {position}\nkind: page\ntags:\n\
         archived: false\n-->\n\n# {title}\n\n{body}\n"
    )
}

/// Push `theirs`'s version, commit `ours`'s, then start the merge with plain
/// git. Returns once the clone is genuinely mid-merge.
fn conflict_on(sb: &GitSandbox, file: &str, ours: &str, theirs: &str) {
    sb.commit_change(&sb.theirs, file, theirs, "Their edit");
    sb.git(&sb.theirs, &["push", "origin", "main"])
        .expect_success("push theirs");
    sb.commit_change(&sb.ours, file, ours, "Our edit");
    start_merge(sb);
}

fn start_merge(sb: &GitSandbox) {
    sb.git(&sb.ours, &["fetch", "origin"])
        .expect_success("fetch");
    let r = sb.git(&sb.ours, &["merge", "origin/main"]);
    assert!(
        !r.success,
        "this merge was supposed to clash; git said: {}{}",
        r.stdout, r.stderr
    );
}

fn parents(sb: &GitSandbox, clone: &Path) -> usize {
    sb.git(clone, &["log", "-1", "--format=%P"])
        .expect_success("log")
        .stdout
        .split_whitespace()
        .count()
}

fn file_at_head(sb: &GitSandbox, clone: &Path, path: &str) -> String {
    sb.git(clone, &["show", &format!("HEAD:{path}")])
        .expect_success("show")
        .stdout
}

// --- classification --------------------------------------------------------

#[test]
fn no_merge_in_progress_reports_nothing() {
    let sb = sandbox();
    let s = synced(&sb);
    assert!(
        s.conflicts().expect("read conflicts").is_none(),
        "a quiet clone has no unfinished merge"
    );
}

#[test]
fn two_different_bodies_are_a_body_conflict() {
    let sb = sandbox();
    let base = fs::read_to_string(sb.ours.join(PLAN_FILE)).unwrap();
    conflict_on(
        &sb,
        PLAN_FILE,
        &base.replace("# Assessment Plan", "# Assessment Plan (ours)"),
        &base.replace("# Assessment Plan", "# Assessment Plan (theirs)"),
    );

    let report = synced(&sb).conflicts().expect("read").expect("mid-merge");
    let f = report
        .files
        .iter()
        .find(|f| f.path == PLAN_FILE)
        .expect("the plan page clashed");
    assert_eq!(f.kind, ConflictKind::Body);
    assert!(!f.kind.is_auto_resolvable());
    // All three sides were read out of the index, not out of the working tree.
    assert!(f.base.as_deref().unwrap().contains("# Assessment Plan"));
    assert!(f.ours.as_deref().unwrap().contains("(ours)"));
    assert!(f.theirs.as_deref().unwrap().contains("(theirs)"));
    assert!(!report.can_finish(), "a page clash blocks the merge");
    assert_eq!(report.blocking_paths(), vec![PLAN_FILE.to_string()]);
}

#[test]
fn same_body_different_metadata_is_a_metadata_conflict() {
    let sb = sandbox();
    let base = fs::read_to_string(sb.ours.join(PLAN_FILE)).unwrap();
    conflict_on(
        &sb,
        PLAN_FILE,
        &base.replace("position: 30", "position: 40"),
        &base.replace("position: 30", "position: 50"),
    );

    let report = synced(&sb).conflicts().expect("read").expect("mid-merge");
    let f = report.files.iter().find(|f| f.path == PLAN_FILE).unwrap();
    assert_eq!(
        f.kind,
        ConflictKind::Metadata,
        "the words are identical; only the ordering metadata differs"
    );
    // Surfaced, never unioned: it still blocks, because position 40 and
    // position 50 are two people's intentions and BerryWiki cannot pick.
    assert!(!f.kind.is_auto_resolvable());
    assert!(!report.can_finish());
}

#[test]
fn both_creating_the_same_page_is_an_add_add_conflict() {
    let sb = sandbox();
    conflict_on(
        &sb,
        "Glossary.md",
        &page(
            "0195f6d0-0000-7000-8000-0000000000aa",
            HOME_ID,
            90,
            "Glossary",
            "Ours.",
        ),
        &page(
            "0195f6d0-0000-7000-8000-0000000000bb",
            HOME_ID,
            91,
            "Glossary",
            "Theirs.",
        ),
    );

    let report = synced(&sb).conflicts().expect("read").expect("mid-merge");
    let f = report
        .files
        .iter()
        .find(|f| f.path == "Glossary.md")
        .unwrap();
    assert_eq!(f.kind, ConflictKind::AddedBoth);
    assert!(f.base.is_none(), "an add/add clash has no common ancestor");
    assert!(f.ours.is_some() && f.theirs.is_some());
}

#[test]
fn edited_here_deleted_there_is_named_as_such() {
    let sb = sandbox();
    let base = fs::read_to_string(sb.ours.join(PLAN_FILE)).unwrap();

    fs::remove_file(sb.theirs.join(PLAN_FILE)).unwrap();
    sb.git(&sb.theirs, &["add", "-A"]).expect_success("stage");
    sb.git(&sb.theirs, &["commit", "-m", "Remove the plan"])
        .expect_success("commit");
    sb.git(&sb.theirs, &["push", "origin", "main"])
        .expect_success("push");

    sb.commit_change(
        &sb.ours,
        PLAN_FILE,
        &base.replace("# Assessment Plan", "# Assessment Plan (kept)"),
        "Keep and edit the plan",
    );
    start_merge(&sb);

    let report = synced(&sb).conflicts().expect("read").expect("mid-merge");
    let f = report.files.iter().find(|f| f.path == PLAN_FILE).unwrap();
    assert_eq!(f.kind, ConflictKind::DeletedByThem);
    assert!(f.theirs.is_none(), "the other side has no version to show");
    assert!(f.ours.as_deref().unwrap().contains("(kept)"));
    assert!(!report.can_finish(), "losing a page is a person's decision");
}

// --- the one clash BerryWiki settles ---------------------------------------

#[test]
fn a_sidebar_only_clash_is_settled_by_regenerating_it() {
    let sb = sandbox();

    // Each side adds a different page and regenerates the navigation file.
    // The pages merge cleanly; the generated file is the only clash.
    fs::write(
        sb.theirs.join("Zither.md"),
        page(
            "0195f6d0-0000-7000-8000-0000000000cc",
            HOME_ID,
            90,
            "Zither",
            "Theirs.",
        ),
    )
    .unwrap();
    fs::write(
        sb.theirs.join("_Sidebar.md"),
        "# Notebook\n\n- [Zither](Zither)\n",
    )
    .unwrap();
    sb.git(&sb.theirs, &["add", "-A"]).expect_success("stage");
    sb.git(&sb.theirs, &["commit", "-m", "Add Zither"])
        .expect_success("commit");
    sb.git(&sb.theirs, &["push", "origin", "main"])
        .expect_success("push");

    fs::write(
        sb.ours.join("Yarrow.md"),
        page(
            "0195f6d0-0000-7000-8000-0000000000dd",
            HOME_ID,
            91,
            "Yarrow",
            "Ours.",
        ),
    )
    .unwrap();
    fs::write(
        sb.ours.join("_Sidebar.md"),
        "# Notebook\n\n- [Yarrow](Yarrow)\n",
    )
    .unwrap();
    sb.git(&sb.ours, &["add", "-A"]).expect_success("stage");
    sb.git(&sb.ours, &["commit", "-m", "Add Yarrow"])
        .expect_success("commit");
    start_merge(&sb);

    let mut s = synced(&sb);
    let report = s.conflicts().expect("read").expect("mid-merge");
    assert_eq!(report.files.len(), 1, "only the generated file clashed");
    assert_eq!(report.files[0].path, "_Sidebar.md");
    assert_eq!(report.files[0].kind, ConflictKind::Sidebar);
    assert!(report.is_sidebar_only());
    assert!(report.can_finish());

    let commit = s.finish_merge().expect("conclude the merge");
    assert_eq!(parents(&sb, &sb.ours), 2, "a merge commit has two parents");
    assert!(
        sb.git(&sb.ours, &["status", "--porcelain"])
            .expect_success("status")
            .stdout
            .trim()
            .is_empty(),
        "the working tree is clean once the merge is concluded"
    );
    assert_eq!(sb.head(&sb.ours), commit.0.clone());

    // The sidebar in the merge commit was regenerated from the merged pages:
    // it names both new pages and carries no conflict markers.
    let sidebar = file_at_head(&sb, &sb.ours, "_Sidebar.md");
    assert!(
        !sidebar.contains("<<<<<<<"),
        "no markers survived: {sidebar}"
    );
    assert!(sidebar.contains("Yarrow"), "our page is listed: {sidebar}");
    assert!(
        sidebar.contains("Zither"),
        "their page is listed: {sidebar}"
    );

    // Both pages themselves survived the merge untouched.
    assert!(file_at_head(&sb, &sb.ours, "Yarrow.md").contains("Ours."));
    assert!(file_at_head(&sb, &sb.ours, "Zither.md").contains("Theirs."));
}

// --- refusals --------------------------------------------------------------

#[test]
fn concluding_is_refused_while_a_page_is_unsettled() {
    let sb = sandbox();
    let base = fs::read_to_string(sb.ours.join(PLAN_FILE)).unwrap();
    conflict_on(
        &sb,
        PLAN_FILE,
        &base.replace("# Assessment Plan", "# Assessment Plan (ours)"),
        &base.replace("# Assessment Plan", "# Assessment Plan (theirs)"),
    );

    let before = sb.head(&sb.ours);
    let mut s = synced(&sb);
    match s.finish_merge() {
        Err(SyncError::UnmergedPaths { paths }) => {
            assert_eq!(paths, vec![PLAN_FILE.to_string()])
        }
        other => panic!("expected a refusal naming the page, got {other:?}"),
    }
    assert_eq!(sb.head(&sb.ours), before, "nothing was committed");
}

#[test]
fn concluding_is_refused_when_no_merge_is_under_way() {
    let sb = sandbox();
    let mut s = synced(&sb);
    assert!(matches!(
        s.finish_merge(),
        Err(SyncError::NoMergeInProgress)
    ));
}

#[test]
fn saving_a_page_is_refused_mid_merge() {
    let sb = sandbox();
    let base = fs::read_to_string(sb.ours.join(PLAN_FILE)).unwrap();
    conflict_on(
        &sb,
        PLAN_FILE,
        &base.replace("# Assessment Plan", "# Assessment Plan (ours)"),
        &base.replace("# Assessment Plan", "# Assessment Plan (theirs)"),
    );

    let before = sb.head(&sb.ours);
    let mut s = synced(&sb);
    match s.update_page("0195f6ec-36a2-7a42-b519-5f558842e256", "New words.") {
        Err(SyncError::MergeInProgress) => {}
        other => panic!("a save mid-merge must be refused, got {other:?}"),
    }
    assert_eq!(sb.head(&sb.ours), before, "nothing was committed");
}
