// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! P3-serve-sync: the editor over a commit-on-save backend, driven through
//! `handle()` against a real bare remote plus two clones
//! (`berrywiki_git_compat::GitSandbox`). Every mutating test asserts the
//! git evidence directly (commit count, files in HEAD, remote tip), never a
//! notice string alone.
//!
//! Plain-mode behaviour is covered by `tests/editor.rs`; here it appears only
//! as the contrast case (no commit, no notice).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;

use berrywiki_git::Identity;
use berrywiki_git_compat::GitSandbox;
use berrywiki_serve::{handle, App, Request, Response};
use berrywiki_store::LocalFolderStore;
use berrywiki_sync::SyncedStore;

const PLAN_ID: &str = "0195f6ec-36a2-7a42-b519-5f558842e256";

// App-state is keyed by the wiki's canonical path, so one temp XDG_STATE_HOME
// shared across tests stays isolated per sandbox. Set once, before any store
// opens, so threaded tests never race on the env var.
static XDG: Once = Once::new();
fn init_xdg() {
    XDG.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bw-serve-sync-xdg-{}", std::process::id()));
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

/// A commit-on-save app over the sandbox's `ours` clone, no draft store.
fn synced_app(sb: &GitSandbox) -> App {
    init_xdg();
    let store = SyncedStore::open_local(&sb.ours, identity()).expect("open synced wiki");
    App::synced_with_drafts(store, None)
}

/// The contrast case: same clone, no git wiring.
fn plain_app(sb: &GitSandbox) -> App {
    init_xdg();
    App::with_drafts(LocalFolderStore::open(&sb.ours).unwrap(), None)
}

fn commit_count(sb: &GitSandbox, clone: &Path) -> usize {
    sb.git(clone, &["rev-list", "--count", "HEAD"])
        .expect_success("rev-list")
        .stdout
        .trim()
        .parse()
        .unwrap()
}

fn subject(sb: &GitSandbox, clone: &Path, rev: &str) -> String {
    sb.git(clone, &["log", "-1", "--format=%s", rev])
        .expect_success("log")
        .stdout
        .trim()
        .to_string()
}

fn files_in(sb: &GitSandbox, clone: &Path, rev: &str) -> Vec<String> {
    sb.git(clone, &["show", "--name-only", "--format=", rev])
        .expect_success("show")
        .stdout
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect()
}

fn is_clean(sb: &GitSandbox, clone: &Path) -> bool {
    sb.git(clone, &["status", "--porcelain"])
        .expect_success("status")
        .stdout
        .trim()
        .is_empty()
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn base_of(html: &str) -> String {
    let marker = "name=\"base\" value=\"";
    let i = html.find(marker).expect("edit form carries a base field") + marker.len();
    html[i..].split('"').next().unwrap().to_string()
}

fn no_script(html: &str) {
    let lower = html.to_lowercase();
    assert!(!lower.contains("<script"), "no script element");
    assert!(!lower.contains("javascript:"), "no javascript: URLs");
    assert!(!lower.contains(" onerror="), "no inline handlers");
    assert!(!lower.contains(" onclick="), "no inline handlers");
}

/// Save new text to the plan page through the editor form.
fn save_plan(app: &mut App, text: &str) -> Response {
    let edit = handle(app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    assert_eq!(edit.status, 200);
    let base = base_of(&edit.body);
    let form = format!("body={}&action=save&base={base}", enc(text));
    handle(app, &Request::post(&format!("/page/{PLAN_ID}/edit"), &form))
}

/// Create a top-level page and return (redirect location, page id).
fn create_page(app: &mut App, title: &str) -> (String, String) {
    let form = format!("title={}&parent=&body=&action=create", enc(title));
    let r = handle(app, &Request::post("/new", &form));
    assert_eq!(r.status, 303, "create answers PRG: {}", r.body);
    let location = r.location.expect("redirect location");
    let id = location
        .strip_prefix("/page/")
        .expect("redirect to the new page")
        .split('?')
        .next()
        .unwrap()
        .to_string();
    (location, id)
}

// ---------- commit-on-save ----------

#[test]
fn create_commits_page_and_sidebar_in_one_commit() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = synced_app(&sb);
    let before = commit_count(&sb, &sb.ours);

    let (location, _id) = create_page(&mut app, "Notes");
    assert!(
        location.ends_with("?notice=committed"),
        "redirect carries the fixed notice token: {location}"
    );
    assert_eq!(
        commit_count(&sb, &sb.ours),
        before + 1,
        "exactly one commit"
    );
    assert_eq!(subject(&sb, &sb.ours, "HEAD"), "Create page \"Notes\"");
    let files = files_in(&sb, &sb.ours, "HEAD");
    assert!(
        files.iter().any(|f| f == "Notes.md"),
        "page file: {files:?}"
    );
    assert!(
        files.iter().any(|f| f == "_Sidebar.md"),
        "sidebar in the same commit: {files:?}"
    );
    assert!(is_clean(&sb, &sb.ours), "nothing left uncommitted");
}

#[test]
fn save_is_one_commit_and_the_page_shows_the_notice() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = synced_app(&sb);
    let before = commit_count(&sb, &sb.ours);

    let r = save_plan(
        &mut app,
        "# Assessment Plan\n\nrevised through the editor\n",
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/page/{PLAN_ID}?notice=committed").as_str())
    );
    assert_eq!(commit_count(&sb, &sb.ours), before + 1);
    assert!(subject(&sb, &sb.ours, "HEAD").starts_with("Update page "));
    assert!(is_clean(&sb, &sb.ours));

    // The notice is rendered from the token, never from query text verbatim.
    let page = handle(
        &mut app,
        &Request::get(&format!("/page/{PLAN_ID}?notice=committed")),
    );
    assert_eq!(page.status, 200);
    assert!(page.body.contains("class=\"notice\""), "notice shown");
    assert!(
        page.body.contains("class=\"status-strip\""),
        "status strip shown"
    );
    no_script(&page.body);
}

#[test]
fn unchanged_save_records_nothing() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = synced_app(&sb);
    let original =
        fs::read_to_string(sb.ours.join("Teaching--Course-A--Assessment-Plan.md")).unwrap();
    // Body only: strip the metadata block the editor never shows.
    let body = original
        .split_once("-->\n")
        .map(|(_, b)| b.trim_start_matches('\n').to_string())
        .unwrap_or(original.clone());
    let before = commit_count(&sb, &sb.ours);

    let r = save_plan(&mut app, &body);
    assert_eq!(r.status, 303, "{}", r.body);
    let location = r.location.unwrap();
    // Either the write was byte-identical (no commit) or the editor normalised
    // line endings (one commit); both are honest, neither is two.
    let after = commit_count(&sb, &sb.ours);
    assert!(
        after == before || after == before + 1,
        "{before} -> {after}"
    );
    if after == before {
        assert!(location.ends_with("?notice=unchanged"), "{location}");
    }
    assert!(is_clean(&sb, &sb.ours));
}

#[test]
fn plain_mode_leaves_the_tree_dirty_and_redirects_without_a_notice() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = plain_app(&sb);
    let before = commit_count(&sb, &sb.ours);

    let r = save_plan(&mut app, "# Assessment Plan\n\nplain save\n");
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/page/{PLAN_ID}").as_str())
    );
    assert_eq!(
        commit_count(&sb, &sb.ours),
        before,
        "no commit in plain mode"
    );
    assert!(
        !is_clean(&sb, &sb.ours),
        "the save is visible to git as a change"
    );

    let page = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}")));
    assert!(page.body.contains("commit-on-save off"), "strip is honest");
    assert!(!page.body.contains("action=\"/sync\""));
}

#[test]
fn outside_change_is_checkpointed_before_the_save_commit() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = synced_app(&sb);
    fs::write(
        sb.ours.join("Loose-Note.md"),
        "# Loose\n\nwritten outside berrywiki\n",
    )
    .unwrap();
    let before = commit_count(&sb, &sb.ours);

    let r = save_plan(&mut app, "# Assessment Plan\n\nafter a loose note\n");
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/page/{PLAN_ID}?notice=committed-after-checkpoint").as_str())
    );
    assert_eq!(commit_count(&sb, &sb.ours), before + 2);
    assert_eq!(
        subject(&sb, &sb.ours, "HEAD~1"),
        "Record changes made outside BerryWiki"
    );
    assert!(files_in(&sb, &sb.ours, "HEAD~1")
        .iter()
        .any(|f| f == "Loose-Note.md"));
    assert!(!files_in(&sb, &sb.ours, "HEAD")
        .iter()
        .any(|f| f == "Loose-Note.md"));
    assert!(fs::read_to_string(sb.ours.join("Loose-Note.md"))
        .unwrap()
        .contains("outside berrywiki"));
}

#[test]
fn delete_is_one_commit_removing_page_and_updating_sidebar() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = synced_app(&sb);
    let (_, id) = create_page(&mut app, "Disposable");
    let before = commit_count(&sb, &sb.ours);

    let r = handle(&mut app, &Request::post(&format!("/page/{id}/delete"), ""));
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(
        r.location
            .as_deref()
            .unwrap_or("")
            .contains("notice=committed"),
        "delete redirect carries the notice: {:?}",
        r.location
    );
    assert_eq!(commit_count(&sb, &sb.ours), before + 1);
    assert_eq!(subject(&sb, &sb.ours, "HEAD"), "Delete page \"Disposable\"");
    let files = files_in(&sb, &sb.ours, "HEAD");
    assert!(files.iter().any(|f| f == "Disposable.md"), "{files:?}");
    assert!(files.iter().any(|f| f == "_Sidebar.md"), "{files:?}");
    assert!(!sb.ours.join("Disposable.md").exists());
    assert!(is_clean(&sb, &sb.ours));
}

// ---------- /changes, /conflicts, POST /sync ----------

#[test]
fn changes_page_lists_pending_state_and_recent_commits() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = synced_app(&sb);
    save_plan(&mut app, "# Assessment Plan\n\nfor the changes page\n");
    let head = sb.head(&sb.ours);

    let r = handle(&mut app, &Request::get("/changes"));
    assert_eq!(r.status, 200);
    assert!(
        r.body.contains(&head[..7]),
        "recent commit listed by short id"
    );
    assert!(r.body.contains("Update page"), "commit subject shown");
    assert!(r.body.contains("action=\"/sync\""), "sync form offered");
    assert!(!r.body.contains("Commit-on-save is off"));
    no_script(&r.body);
}

#[test]
fn sync_publishes_local_commits_to_the_remote() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = synced_app(&sb);
    save_plan(&mut app, "# Assessment Plan\n\npublish me\n");
    let our_tip = sb.head(&sb.ours);
    assert_ne!(sb.head(&sb.remote), our_tip, "not yet published");

    let r = handle(&mut app, &Request::post("/sync", ""));
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.location.as_deref(),
        Some("/changes?notice=synced-published")
    );
    assert_eq!(
        sb.head(&sb.remote),
        our_tip,
        "remote fast-forwarded to ours"
    );

    let after = handle(&mut app, &Request::get("/changes?notice=synced-published"));
    assert_eq!(after.status, 200);
    assert!(after.body.contains("class=\"notice\""));
    no_script(&after.body);

    // A second sync has nothing to do and says so.
    let again = handle(&mut app, &Request::post("/sync", ""));
    assert_eq!(
        again.location.as_deref(),
        Some("/changes?notice=synced-up-to-date")
    );
}

#[test]
fn diverged_sync_hands_off_to_the_conflicts_page_and_touches_nothing() {
    let sb = GitSandbox::create(&fixture_dir());
    // Someone else publishes first.
    sb.commit_change(
        &sb.theirs,
        "Research.md",
        "# Research\n\ntheir change\n",
        "Their edit",
    );
    sb.git(&sb.theirs, &["push", "origin", "main"])
        .expect_success("push theirs");
    let their_tip = sb.head(&sb.theirs);

    let mut app = synced_app(&sb);
    save_plan(&mut app, "# Assessment Plan\n\nour change\n");
    let our_tip = sb.head(&sb.ours);

    let r = handle(&mut app, &Request::post("/sync", ""));
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(r.location.as_deref(), Some("/conflicts"));
    assert_eq!(sb.head(&sb.remote), their_tip, "remote untouched");
    assert_eq!(
        sb.head(&sb.ours),
        our_tip,
        "local untouched: no merge attempted"
    );
    assert!(is_clean(&sb, &sb.ours));

    let page = handle(&mut app, &Request::get("/conflicts"));
    assert_eq!(page.status, 200);
    assert!(page.body.contains(&our_tip[..7]), "local tip named");
    assert!(page.body.contains(&their_tip[..7]), "upstream tip named");
    assert!(page.body.contains("git fetch"), "manual steps given");
    assert!(
        !page.body.contains("--force"),
        "never suggests a force push"
    );
    no_script(&page.body);
}

#[test]
fn detached_head_refuses_the_save_and_keeps_the_text() {
    let sb = GitSandbox::create(&fixture_dir());
    sb.git(&sb.ours, &["checkout", "--detach"])
        .expect_success("detach in the sandbox");
    let mut app = synced_app(&sb);
    let before = commit_count(&sb, &sb.ours);

    let text = "# Assessment Plan\n\ntyped while detached\n";
    let r = save_plan(&mut app, text);
    assert_eq!(r.status, 409, "refused, not silently dropped: {}", r.body);
    assert!(
        r.body.contains("typed while detached"),
        "submitted text kept"
    );
    assert_eq!(commit_count(&sb, &sb.ours), before, "no commit");
    no_script(&r.body);
}

#[test]
fn every_synced_route_is_script_free() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = synced_app(&sb);
    for target in [
        "/",
        "/changes",
        "/changes?notice=synced-integrated",
        "/conflicts",
        "/diagnostics",
        "/search?q=e",
        &format!("/page/{PLAN_ID}"),
        &format!("/page/{PLAN_ID}?notice=committed-after-checkpoint"),
        &format!("/page/{PLAN_ID}/edit"),
        "/new",
    ] {
        let r = handle(&mut app, &Request::get(target));
        assert!(r.status < 500, "{target}: {}", r.status);
        no_script(&r.body);
    }
}

#[test]
fn sync_on_a_plain_app_is_refused_not_faked() {
    let sb = GitSandbox::create(&fixture_dir());
    let mut app = plain_app(&sb);
    let r = handle(&mut app, &Request::post("/sync", ""));
    assert_eq!(r.status, 400);
    assert!(r.body.contains("Sync unavailable"));
    no_script(&r.body);
}
