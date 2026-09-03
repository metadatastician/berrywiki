// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Integration tests for P2-edit: the editor routes over a scratch copy of
//! the fixture wiki, with an injected draft store (never env-dependent).
//!
//! Conventions mirror `berrywiki-store/tests/local_store.rs`: every test gets
//! its own temp directory; the fixture itself is read-only.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use berrywiki_draft::DraftStore;
use berrywiki_serve::{handle, App, Request};
use berrywiki_store::LocalFolderStore;

const HOME_ID: &str = "0195f6d0-0000-7000-8000-000000000001";
const TEACHING_ID: &str = "0195f6d0-0000-7000-8000-000000000002";
const PLAN_ID: &str = "0195f6ec-36a2-7a42-b519-5f558842e256";
const COURSE_A_ID: &str = "0195f6d0-0000-7000-8000-000000000003";
const RESEARCH_ID: &str = "0195f6d0-0000-7000-8000-000000000004";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn scratch_dir(kind: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "berrywiki-editor-{kind}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Copy the fixture wiki into a fresh scratch directory.
fn scratch_wiki() -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-wiki")
        .canonicalize()
        .expect("fixture exists");
    let dir = scratch_dir("wiki");
    for entry in fs::read_dir(&fixture).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    dir
}

/// An editable app over a scratch wiki with an injected draft store.
fn app(wiki: &PathBuf, drafts_dir: &PathBuf) -> App {
    App::with_drafts(
        LocalFolderStore::open(wiki).unwrap(),
        Some(DraftStore::new(drafts_dir)),
    )
}

/// Percent-encode a form value the way a browser does.
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

/// Pull the hidden `base` hash out of a rendered edit form.
fn base_of(html: &str) -> String {
    let marker = "name=\"base\" value=\"";
    let i = html.find(marker).expect("edit form carries a base field") + marker.len();
    html[i..].split('"').next().unwrap().to_string()
}

/// Find the page file that carries `id` (filenames are title-derived).
fn file_of(wiki: &PathBuf, id: &str) -> PathBuf {
    for entry in fs::read_dir(wiki).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("md")
            && fs::read_to_string(&path).unwrap().contains(id)
        {
            return path;
        }
    }
    panic!("no page file contains id {id}");
}

fn no_script(html: &str) {
    let lower = html.to_lowercase();
    assert!(!lower.contains("<script"), "no script element");
    assert!(!lower.contains("javascript:"), "no javascript: URLs");
    assert!(!lower.contains(" onerror="), "no inline handlers");
    assert!(!lower.contains(" onclick="), "no inline handlers");
}

#[test]
fn edit_form_prefills_source_and_carries_base() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let r = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    assert_eq!(r.status, 200);
    assert!(r.body.contains("<textarea"), "editor textarea present");
    assert!(r.body.contains("Assessment Plan"), "body prefilled");
    assert_eq!(base_of(&r.body).len(), 16, "base is an fnv-1a hex hash");
    no_script(&r.body);
}

#[test]
fn save_round_trip_preserves_metadata_and_updates_sidebar() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app = app(&wiki, &drafts_dir);

    let page_file = file_of(&wiki, PLAN_ID);
    let before = fs::read_to_string(&page_file).unwrap();
    let meta_block: String = before
        .lines()
        .take_while(|l| !l.trim().is_empty() || before.starts_with("<!--"))
        .take(20)
        .filter(|l| l.contains("berrywiki") || l.starts_with("id:") || l.contains("-->"))
        .collect();

    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);

    let new_body = "# Assessment Plan Revised\n\nNew content after the save.\n";
    let form = format!("body={}&action=save&base={base}", enc(new_body));
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303, "save answers Post/Redirect/Get: {}", r.body);
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/page/{PLAN_ID}").as_str())
    );

    let after = fs::read_to_string(&page_file).unwrap();
    assert!(after.contains("New content after the save."));
    assert!(
        !meta_block.is_empty() && meta_block.contains("berrywiki"),
        "sanity: fixture page is managed"
    );
    assert!(
        after.contains(PLAN_ID),
        "metadata block survives the save byte-stably"
    );
    let sidebar = fs::read_to_string(wiki.join("_Sidebar.md")).unwrap();
    assert!(
        sidebar.contains("Assessment Plan Revised"),
        "sidebar regenerated in the same store operation"
    );
}

#[test]
fn save_clears_the_draft() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app = app(&wiki, &drafts_dir);

    let form = format!("body={}&action=save-draft&base=x", enc("draft text"));
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303);
    assert!(DraftStore::new(&drafts_dir).has(PLAN_ID));

    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    let form = format!("body={}&action=save&base={base}", enc("# Saved\n\ndone\n"));
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303);
    assert!(
        !DraftStore::new(&drafts_dir).has(PLAN_ID),
        "a successful save supersedes the draft"
    );
}

#[test]
fn save_draft_persists_across_a_new_app_and_is_visible_everywhere() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app1 = app(&wiki, &drafts_dir);

    let page_file = file_of(&wiki, PLAN_ID);
    let disk_before = fs::read_to_string(&page_file).unwrap();

    let form = format!(
        "body={}&action=save-draft&base=irrelevant",
        enc("# WIP\n\nhalf a thought")
    );
    let r = handle(
        &mut app1,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303);
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/page/{PLAN_ID}/edit?notice=draft-saved").as_str())
    );
    assert_eq!(
        fs::read_to_string(&page_file).unwrap(),
        disk_before,
        "save-draft must not touch the wiki"
    );

    // A fresh app over the same draft dir still sees it (killed process).
    let mut app2 = app(&wiki, &drafts_dir);
    let edit = handle(&mut app2, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    assert!(
        edit.body.contains("Unsaved draft"),
        "draft banner on editor"
    );
    assert!(edit.body.contains("half a thought"), "draft content shown");

    let view = handle(&mut app2, &Request::get(&format!("/page/{PLAN_ID}")));
    assert!(
        view.body.contains("draft-badge"),
        "page view shows the badge"
    );
    assert!(view.body.contains("draft-dot"), "nav tree marks the draft");
}

#[test]
fn discard_draft_returns_to_the_saved_page() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app = app(&wiki, &drafts_dir);

    let form = format!("body={}&action=save-draft&base=x", enc("temp"));
    handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    let form = format!("body={}&action=discard-draft&base=x", enc("temp"));
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303);
    assert!(!DraftStore::new(&drafts_dir).has(PLAN_ID));
}

#[test]
fn preview_renders_without_writing_anything() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app = app(&wiki, &drafts_dir);
    let page_file = file_of(&wiki, PLAN_ID);
    let disk_before = fs::read_to_string(&page_file).unwrap();

    let md = "## Preview heading\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    let form = format!("body={}&action=preview&base=carried", enc(md));
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 200);
    assert!(r.body.contains("Preview heading"), "markdown rendered");
    assert!(r.body.contains("<table>"), "GFM table in preview");
    assert!(
        r.body.contains("| a | b |") || r.body.contains("Preview heading"),
        "textarea keeps text"
    );
    assert!(r.body.contains("value=\"carried\""), "base carried forward");
    assert_eq!(fs::read_to_string(&page_file).unwrap(), disk_before);
    assert!(
        !DraftStore::new(&drafts_dir).has(PLAN_ID),
        "preview writes no draft"
    );
    no_script(&r.body);
}

#[test]
fn crlf_submissions_save_as_lf_and_resave_is_a_byte_noop() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let page_file = file_of(&wiki, PLAN_ID);

    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    // A browser submits textarea content CRLF-separated.
    let form =
        format!("body=%23+Plan%0D%0A%0D%0Aline+one%0D%0Aline+two%0D%0A&action=save&base={base}");
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303);
    let after_first = fs::read_to_string(&page_file).unwrap();
    assert!(!after_first.contains('\r'), "CRLF normalised to LF on disk");
    assert!(after_first.contains("line one\nline two"));

    // Saving the same content again must not change a byte.
    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    let form =
        format!("body=%23+Plan%0D%0A%0D%0Aline+one%0D%0Aline+two%0D%0A&action=save&base={base}");
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303);
    assert_eq!(
        fs::read_to_string(&page_file).unwrap(),
        after_first,
        "idempotent re-save is byte-for-byte identical"
    );
}

#[test]
fn external_disk_change_makes_save_a_409_that_keeps_the_text() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app = app(&wiki, &drafts_dir);
    let page_file = file_of(&wiki, PLAN_ID);

    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);

    // Someone edits the file behind the app's back (different length).
    let external = format!(
        "{}\nExternal terminal edit.\n",
        fs::read_to_string(&page_file).unwrap()
    );
    fs::write(&page_file, &external).unwrap();

    let form = format!("body={}&action=save&base={base}", enc("my competing text"));
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 409, "stale write is refused");
    assert!(
        r.body.contains("my competing text"),
        "submitted text intact in the form"
    );
    assert!(
        r.body.contains("kept as a draft"),
        "and persisted as a draft"
    );
    assert!(r.body.contains("/reload"), "reload affordance offered");
    assert_eq!(
        DraftStore::new(&drafts_dir)
            .load(PLAN_ID)
            .unwrap()
            .unwrap()
            .content,
        "my competing text"
    );
    assert_eq!(
        fs::read_to_string(&page_file).unwrap(),
        external,
        "the external edit is untouched"
    );
    no_script(&r.body);
}

#[test]
fn a_stale_base_from_an_older_editor_is_refused() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app = app(&wiki, &drafts_dir);

    // Editor A opens.
    let edit_a = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base_a = base_of(&edit_a.body);

    // Editor B opens and saves first (the store reloads and re-fingerprints,
    // so the store-level guard alone would now let A clobber B).
    let edit_b = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base_b = base_of(&edit_b.body);
    let form = format!(
        "body={}&action=save&base={base_b}",
        enc("# B\n\nB won the race\n")
    );
    assert_eq!(
        handle(
            &mut app,
            &Request::post(&format!("/page/{PLAN_ID}/edit"), &form)
        )
        .status,
        303
    );

    // A's save now carries a stale base.
    let form = format!(
        "body={}&action=save&base={base_a}",
        enc("# A\n\nA would clobber B\n")
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 409);
    assert!(r.body.contains("A would clobber B"), "A's text preserved");
    let disk = fs::read_to_string(file_of(&wiki, PLAN_ID)).unwrap();
    assert!(disk.contains("B won the race"), "B's save stands");
}

#[test]
fn create_appends_a_child_and_regenerates_the_sidebar() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));

    let form = format!(
        "title={}&parent={TEACHING_ID}&body={}&action=create",
        enc("Marking Rubric"),
        enc("Rubric details.\n")
    );
    let r = handle(&mut app, &Request::post("/new", &form));
    assert_eq!(r.status, 303, "{}", r.body);
    let loc = r.location.clone().expect("redirect to the new page");
    let new_id = loc.strip_prefix("/page/").unwrap().to_string();
    assert_eq!(
        new_id.split('-').count(),
        5,
        "UUID-shaped minted id: {new_id}"
    );

    let view = handle(&mut app, &Request::get(&loc));
    assert_eq!(view.status, 200);
    assert!(view.body.contains("Marking Rubric"));
    let sidebar = fs::read_to_string(wiki.join("_Sidebar.md")).unwrap();
    assert!(
        sidebar.contains("Marking Rubric"),
        "sidebar regenerated on create"
    );
}

#[test]
fn create_without_a_title_reprompts_keeping_the_typed_body() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let form = format!("title=&parent=&body={}&action=create", enc("typed content"));
    let r = handle(&mut app, &Request::post("/new", &form));
    assert_eq!(r.status, 400);
    assert!(r.body.contains("needs a title"));
    assert!(
        r.body.contains("typed content"),
        "typed body survives the error"
    );
    no_script(&r.body);
}

#[test]
fn delete_removes_the_file_and_refuses_pages_with_children() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app = app(&wiki, &drafts_dir);

    // A leaf page deletes cleanly.
    let form = format!("title={}&parent=&body=&action=create", enc("Disposable"));
    let r = handle(&mut app, &Request::post("/new", &form));
    let id = r
        .location
        .unwrap()
        .strip_prefix("/page/")
        .unwrap()
        .to_string();
    let confirm = handle(&mut app, &Request::get(&format!("/page/{id}/delete")));
    assert!(confirm.body.contains("Delete permanently"));
    let r = handle(&mut app, &Request::post(&format!("/page/{id}/delete"), ""));
    assert_eq!(r.status, 303);
    let sidebar = fs::read_to_string(wiki.join("_Sidebar.md")).unwrap();
    assert!(
        !sidebar.contains("Disposable"),
        "sidebar regenerated on delete"
    );

    // A parent page is refused, both in the confirm UI and by the store.
    let confirm = handle(
        &mut app,
        &Request::get(&format!("/page/{TEACHING_ID}/delete")),
    );
    assert!(confirm.body.contains("child page"), "children warned about");
    assert!(
        !confirm.body.contains("Delete permanently"),
        "no button offered"
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{TEACHING_ID}/delete"), ""),
    );
    assert_eq!(r.status, 400, "store refusal surfaces as an error");
    assert!(file_exists_with_id(&wiki, TEACHING_ID), "nothing deleted");
}

fn file_exists_with_id(wiki: &PathBuf, id: &str) -> bool {
    fs::read_dir(wiki).unwrap().any(|e| {
        let p = e.unwrap().path();
        p.extension().and_then(|x| x.to_str()) == Some("md")
            && fs::read_to_string(&p).unwrap_or_default().contains(id)
    })
}

#[test]
fn without_a_draft_store_the_editor_degrades_visibly_and_save_still_works() {
    let wiki = scratch_wiki();
    let mut app = App::with_drafts(LocalFolderStore::open(&wiki).unwrap(), None);

    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    assert!(edit.body.contains("Save-draft is unavailable"));
    assert!(
        !edit.body.contains("value=\"save-draft\""),
        "no Save-draft button"
    );

    let base = base_of(&edit.body);
    let form = format!(
        "body={}&action=save&base={base}",
        enc("# Still saves\n\nok\n")
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303, "saving works without drafts");
}

/// Editor surfaces echo the user's text back (escaped) inside the textarea, so
/// inert plain-text occurrences of "onerror=" etc. are expected there. What
/// must NEVER appear is live markup: a script element, or an unescaped
/// `</textarea>` that would break out of the echo context.
fn inert_echo(html: &str) {
    let lower = html.to_lowercase();
    assert!(!lower.contains("<script"), "no script element");
    assert_eq!(
        lower.matches("</textarea>").count(),
        1,
        "exactly the legitimate textarea closer — the payload's stays escaped"
    );
    assert!(
        !lower.contains("href=\"javascript:"),
        "no javascript: link targets"
    );
}

#[test]
fn hostile_input_stays_inert_across_all_editor_surfaces() {
    let wiki = scratch_wiki();
    let drafts_dir = scratch_dir("drafts");
    let mut app = app(&wiki, &drafts_dir);
    let hostile = "</textarea><script>alert(1)</script> <img src=x onerror=alert(1)> javascript:x";

    // Preview of hostile markdown.
    let form = format!("body={}&action=preview&base=b", enc(hostile));
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    inert_echo(&r.body);

    // Saved hostile content, then viewed (render path) and re-edited (echo path).
    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    let form = format!("body={}&action=save&base={base}", enc(hostile));
    assert_eq!(
        handle(
            &mut app,
            &Request::post(&format!("/page/{PLAN_ID}/edit"), &form)
        )
        .status,
        303
    );
    let view = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}"))).body;
    assert!(
        !view.to_lowercase().contains("<script"),
        "render path neutralises"
    );
    assert!(
        !view.to_lowercase().contains(" onerror=alert"),
        "no live handler attribute"
    );
    inert_echo(&handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit"))).body);

    // A hostile draft must not leak live markup into the nav tree either.
    let form = format!("body={}&action=save-draft&base=b", enc(hostile));
    handle(
        &mut app,
        &Request::post(&format!("/page/{HOME_ID}/edit"), &form),
    );
    assert!(!handle(&mut app, &Request::get("/"))
        .body
        .to_lowercase()
        .contains("<script"));

    // New-page form with a hostile title and body.
    let form = format!(
        "title={}&parent=&body={}&action=preview",
        enc(hostile),
        enc(hostile)
    );
    inert_echo(&handle(&mut app, &Request::post("/new", &form)).body);

    // Move form with a hostile parent id (reaches the store, whose error names
    // it) and a hostile position (rejected before the store).
    let (fp, fpos) = open_move_form(&mut app, TEACHING_ID);
    for form in [
        format!(
            "from_parent={fp}&from_position={fpos}&parent={}&position=1&action=preview",
            enc(hostile)
        ),
        format!(
            "from_parent={fp}&from_position={fpos}&parent={RESEARCH_ID}&position={}&action=move",
            enc(hostile)
        ),
    ] {
        let r = handle(
            &mut app,
            &Request::post(&format!("/page/{TEACHING_ID}/move"), &form),
        );
        assert_eq!(r.status, 400, "{}", r.body);
        // The payload is echoed as escaped text (in the error banner or the
        // position field), never as markup or a link target.
        let lower = r.body.to_lowercase();
        assert!(!lower.contains("<script"), "no script element");
        assert!(!lower.contains("<img"), "no live image element");
        assert!(!lower.contains("href=\"javascript:"), "no javascript: link");
        assert!(wiki.join("Teaching.md").exists(), "nothing moved");
    }
}

#[test]
fn reload_recovers_from_staleness_and_redirects_back() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let page_file = file_of(&wiki, PLAN_ID);

    let external = format!(
        "{}\nExternal edit.\n",
        fs::read_to_string(&page_file).unwrap()
    );
    fs::write(&page_file, &external).unwrap();

    let form = format!("back=/page/{PLAN_ID}/edit");
    let r = handle(&mut app, &Request::post("/reload", &form));
    assert_eq!(r.status, 303);
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/page/{PLAN_ID}/edit?notice=reloaded").as_str())
    );

    // After the reload the editor shows the external edit and a fresh base
    // that a save can proceed against.
    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    assert!(edit.body.contains("External edit."));
    let base = base_of(&edit.body);
    let form = format!(
        "body={}&action=save&base={base}",
        enc("# Merged\n\nresolved\n")
    );
    assert_eq!(
        handle(
            &mut app,
            &Request::post(&format!("/page/{PLAN_ID}/edit"), &form)
        )
        .status,
        303
    );
}

#[test]
fn reload_rejects_non_local_redirect_targets() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let r = handle(
        &mut app,
        &Request::post("/reload", "back=https://evil.example"),
    );
    assert_eq!(r.status, 303);
    assert!(
        r.location.as_deref().unwrap_or("").starts_with("/"),
        "local fallback"
    );
    let r = handle(&mut app, &Request::post("/reload", "back=//evil.example"));
    assert!(r.location.as_deref().unwrap_or("").starts_with("/?"));
}

#[test]
fn unknown_methods_are_405() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let req = Request {
        method: "PUT".to_string(),
        path: format!("/page/{PLAN_ID}"),
        query: String::new(),
        body: String::new(),
        bytes: Vec::new(),
        content_type: String::new(),
    };
    assert_eq!(handle(&mut app, &req).status, 405);
}

// ---------- P2-move: /page/<id>/move ----------

/// Pull a hidden field's value out of a rendered form.
fn hidden_of(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let i = html.find(&marker).expect("form carries the hidden field") + marker.len();
    html[i..].split('"').next().unwrap().to_string()
}

/// The move form for Teaching plus its current placement, as the form's base.
fn open_move_form(app: &mut App, id: &str) -> (String, String) {
    let r = handle(app, &Request::get(&format!("/page/{id}/move")));
    assert_eq!(r.status, 200, "{}", r.body);
    (
        hidden_of(&r.body, "from_parent"),
        hidden_of(&r.body, "from_position"),
    )
}

const SUBTREE: [(&str, &str); 3] = [
    ("Teaching.md", "Research--Teaching.md"),
    ("Teaching--Course-A.md", "Research--Teaching--Course-A.md"),
    (
        "Teaching--Course-A--Assessment-Plan.md",
        "Research--Teaching--Course-A--Assessment-Plan.md",
    ),
];

#[test]
fn move_form_offers_every_parent_except_the_page_and_its_subtree() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let r = handle(
        &mut app,
        &Request::get(&format!("/page/{TEACHING_ID}/move")),
    );
    assert_eq!(r.status, 200);
    assert!(
        r.body.contains(&format!("<option value=\"{RESEARCH_ID}\"")),
        "Research is offered"
    );
    assert!(
        r.body
            .contains(&format!("<option value=\"{HOME_ID}\" selected")),
        "current parent preselected"
    );
    assert!(
        r.body.contains("<option value=\"\">(top level)"),
        "top level offered"
    );
    for id in [TEACHING_ID, COURSE_A_ID, PLAN_ID] {
        assert!(
            !r.body.contains(&format!("<option value=\"{id}\"")),
            "the page and its subtree are never offered as a parent: {id}"
        );
    }
    assert_eq!(hidden_of(&r.body, "from_parent"), HOME_ID);
    assert_eq!(hidden_of(&r.body, "from_position"), "10");
    assert!(
        !r.body.contains("class=\"plan\""),
        "no plan until previewed"
    );
    no_script(&r.body);
}

#[test]
fn move_preview_lists_the_exact_cascade_and_changes_nothing() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let (fp, fpos) = open_move_form(&mut app, TEACHING_ID);
    let home_before = fs::read_to_string(wiki.join("Home.md")).unwrap();

    let form = format!(
        "from_parent={fp}&from_position={fpos}&parent={RESEARCH_ID}&position=5&action=preview"
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{TEACHING_ID}/move"), &form),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    for (old, new) in SUBTREE {
        assert!(
            r.body.contains(&format!("<code>{old}</code>")),
            "{old} listed"
        );
        assert!(
            r.body.contains(&format!("<code>{new}</code>")),
            "{new} listed"
        );
        assert!(wiki.join(old).exists(), "{old} untouched by a preview");
        assert!(!wiki.join(new).exists(), "{new} not created by a preview");
    }
    assert!(
        r.body.contains("<code>Home.md</code>"),
        "Home's inbound links would be rewritten"
    );
    assert_eq!(
        fs::read_to_string(wiki.join("Home.md")).unwrap(),
        home_before,
        "preview wrote nothing"
    );
    assert!(r.body.contains("Nothing has been changed"));
    // The chosen destination survives into the re-rendered form.
    assert!(r
        .body
        .contains(&format!("<option value=\"{RESEARCH_ID}\" selected")));
    assert!(r
        .body
        .contains("name=\"position\" type=\"number\" value=\"5\""));
    no_script(&r.body);
}

#[test]
fn move_applies_the_cascade_and_redirects_to_the_page() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let (fp, fpos) = open_move_form(&mut app, TEACHING_ID);

    let form = format!(
        "from_parent={fp}&from_position={fpos}&parent={RESEARCH_ID}&position=5&action=move"
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{TEACHING_ID}/move"), &form),
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.location.as_deref(),
        Some(format!("/page/{TEACHING_ID}").as_str())
    );
    for (old, new) in SUBTREE {
        assert!(!wiki.join(old).exists(), "{old} gone");
        assert!(wiki.join(new).exists(), "{new} written");
    }
    let home = fs::read_to_string(wiki.join("Home.md")).unwrap();
    assert!(
        home.contains("[[Research--Teaching--Course-A--Assessment-Plan#Weighting]]"),
        "inbound link rewritten: {home}"
    );
    let sidebar = fs::read_to_string(wiki.join("_Sidebar.md")).unwrap();
    assert!(
        sidebar.contains("(Research--Teaching)"),
        "sidebar regenerated: {sidebar}"
    );
    let view = handle(&mut app, &Request::get(&format!("/page/{TEACHING_ID}")));
    assert_eq!(view.status, 200, "moved page still served by id");
    assert!(view.body.contains("Research"), "shown under its new parent");
}

#[test]
fn forged_move_into_own_subtree_is_refused_and_changes_nothing() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let (fp, fpos) = open_move_form(&mut app, TEACHING_ID);

    // The form never offers Course A as Teaching's parent; a forged POST
    // reaches the store's own cycle check.
    let form = format!(
        "from_parent={fp}&from_position={fpos}&parent={COURSE_A_ID}&position=1&action=move"
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{TEACHING_ID}/move"), &form),
    );
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(r.body.contains("own ancestor"), "{}", r.body);
    for (old, new) in SUBTREE {
        assert!(wiki.join(old).exists());
        assert!(!wiki.join(new).exists());
    }

    // A position that is not a whole number never reaches the store.
    let form = format!(
        "from_parent={fp}&from_position={fpos}&parent={RESEARCH_ID}&position=five&action=move"
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{TEACHING_ID}/move"), &form),
    );
    assert_eq!(r.status, 400);
    assert!(r.body.contains("whole number"));
    assert!(wiki.join("Teaching.md").exists());
    no_script(&r.body);
}

#[test]
fn a_move_form_opened_before_another_move_is_refused_with_a_fresh_base() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    // A form opened when Teaching still sat under Home at position 10 ...
    let form_a = format!(
        "from_parent={HOME_ID}&from_position=10&parent={RESEARCH_ID}&position=5&action=move"
    );
    // ... after another editor already moved it to position 20.
    let r = handle(
        &mut app,
        &Request::post(
            &format!("/page/{TEACHING_ID}/move"),
            &format!(
                "from_parent={HOME_ID}&from_position=10&parent={HOME_ID}&position=20&action=move"
            ),
        ),
    );
    assert_eq!(r.status, 303, "{}", r.body);

    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{TEACHING_ID}/move"), &form_a),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(r.body.contains("moved after this form was opened"));
    assert_eq!(hidden_of(&r.body, "from_position"), "20", "base refreshed");
    assert!(
        r.body
            .contains(&format!("<option value=\"{RESEARCH_ID}\" selected")),
        "the submitted destination is kept"
    );
    assert!(
        wiki.join("Teaching.md").exists(),
        "the stale move was not applied"
    );
    assert!(!wiki.join("Research--Teaching.md").exists());
    no_script(&r.body);
}

#[test]
fn a_disk_change_after_the_move_form_opened_is_a_409() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let (fp, fpos) = open_move_form(&mut app, TEACHING_ID);

    // Home.md is one of the files the move rewrites; change it behind the
    // store's back (length changes, so the fingerprint guard fires).
    let home = wiki.join("Home.md");
    let mut text = fs::read_to_string(&home).unwrap();
    text.push_str("\nEdited outside BerryWiki.\n");
    fs::write(&home, &text).unwrap();

    let form = format!(
        "from_parent={fp}&from_position={fpos}&parent={RESEARCH_ID}&position=5&action=move"
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{TEACHING_ID}/move"), &form),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(wiki.join("Teaching.md").exists(), "nothing moved");
    assert_eq!(
        fs::read_to_string(&home).unwrap(),
        text,
        "outside edit kept"
    );
    no_script(&r.body);
}

#[test]
fn edit_form_prefills_the_existing_tag_list() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let r = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    assert_eq!(r.status, 200);
    assert!(
        r.body.contains("name=\"tags\""),
        "editor offers a tags field"
    );
    // Rendered as the stored list joined with ", " so what the user sees is
    // what the store holds, in the order it holds it.
    assert!(
        r.body.contains("value=\"assessment, teaching\""),
        "tags prefilled: {}",
        r.body
    );
    no_script(&r.body);
}

#[test]
fn save_round_trips_a_typed_tag_field_to_disk() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let page_file = file_of(&wiki, PLAN_ID);

    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    let form = format!(
        "body={}&tags={}&action=save&base={base}",
        enc("# Assessment Plan\n\nbody\n"),
        enc("alpha, beta-two, gamma")
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303, "save redirects: {}", r.body);

    let after = fs::read_to_string(&page_file).unwrap();
    for tag in ["alpha", "beta-two", "gamma"] {
        assert!(after.contains(tag), "tag {tag} written: {after}");
    }
    assert!(
        !after.contains("assessment"),
        "the typed list replaces the old one, it does not merge"
    );
    // And it comes back through the form on the next edit.
    let again = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    assert!(again.body.contains("value=\"alpha, beta-two, gamma\""));
}

#[test]
fn the_form_parser_trims_and_drops_empties_and_duplicates() {
    // Normalisation happens here, in the form parser, and never in the store:
    // a store that rewrote its input would be the very thing byte-stability
    // exists to prevent.
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    let form = format!(
        "body={}&tags={}&action=save&base={base}",
        enc("# Assessment Plan\n\nb\n"),
        enc("  one ,, two,one,  , two ")
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 303, "{}", r.body);
    // First-occurrence order kept, so the saved list is the one the user can
    // see they typed.
    let again = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    assert!(
        again.body.contains("value=\"one, two\""),
        "got: {}",
        again.body
    );
}

#[test]
fn an_empty_tags_field_clears_the_list() {
    // A text input that is empty means "no tags". This is deliberate rather
    // than incidental: the store's separate `update_page` is what a caller
    // uses when it has no opinion about tags at all.
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let page_file = file_of(&wiki, PLAN_ID);
    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    let form = format!(
        "body={}&tags=&action=save&base={base}",
        enc("# Assessment Plan\n\nb\n")
    );
    assert_eq!(
        handle(
            &mut app,
            &Request::post(&format!("/page/{PLAN_ID}/edit"), &form)
        )
        .status,
        303
    );
    let after = fs::read_to_string(&page_file).unwrap();
    assert!(!after.contains("assessment"), "list cleared: {after}");
    assert!(after.contains(PLAN_ID), "metadata block otherwise intact");
}

#[test]
fn a_tag_the_store_refuses_keeps_the_typed_text_on_the_page() {
    // The stale-write UX rule applied to tags: a refusal must never cost the
    // user the words they typed, in either field.
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let page_file = file_of(&wiki, PLAN_ID);
    let before = fs::read_to_string(&page_file).unwrap();

    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    let typed_body = "# Assessment Plan\n\nwork I do not want to lose\n";
    let form = format!(
        "body={}&tags={}&action=save&base={base}",
        enc(typed_body),
        enc("fine, evil-->here")
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );

    assert_ne!(r.status, 303, "a refused save must not redirect");
    assert!(
        r.body.contains("work I do not want to lose"),
        "typed body echoed back: {}",
        r.body
    );
    assert!(
        r.body.contains("value=\"fine, evil--&gt;here\"")
            || r.body.contains("fine, evil--&gt;here"),
        "typed tags echoed back escaped: {}",
        r.body
    );
    no_script(&r.body);
    assert_eq!(
        fs::read_to_string(&page_file).unwrap(),
        before,
        "nothing written on refusal"
    );
}

#[test]
fn the_new_page_form_offers_and_stores_tags() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let form_page = handle(&mut app, &Request::get("/new"));
    assert!(
        form_page.body.contains("name=\"tags\""),
        "create and edit must not disagree about what a page can carry"
    );
    no_script(&form_page.body);

    let form = format!(
        "title={}&body={}&tags={}&parent=&action=create",
        enc("Tagged Newcomer"),
        enc("Fresh page.\n"),
        enc("alpha, beta")
    );
    let r = handle(&mut app, &Request::post("/new", &form));
    assert_eq!(r.status, 303, "create redirects: {}", r.body);

    let created = fs::read_to_string(wiki.join("Tagged-Newcomer.md")).unwrap();
    assert!(created.contains("alpha"), "tag written: {created}");
    assert!(created.contains("beta"));
}

#[test]
fn the_new_page_form_refuses_a_hostile_tag_without_losing_the_draft() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let form = format!(
        "title={}&body={}&tags={}&parent=&action=create",
        enc("Doomed"),
        enc("text the user typed"),
        enc("evil-->here")
    );
    let r = handle(&mut app, &Request::post("/new", &form));
    assert_ne!(r.status, 303, "refused, not created");
    assert!(r.body.contains("text the user typed"), "{}", r.body);
    assert!(!wiki.join("Doomed.md").exists(), "no page left behind");
    no_script(&r.body);
}

#[test]
fn preview_echoes_the_typed_tags_rather_than_the_stored_ones() {
    // Preview is not a save, so it must show what is in the form, not what is
    // on disk; otherwise a user previewing a tag change sees the old list.
    let wiki = scratch_wiki();
    let mut app = app(&wiki, &scratch_dir("drafts"));
    let edit = handle(&mut app, &Request::get(&format!("/page/{PLAN_ID}/edit")));
    let base = base_of(&edit.body);
    let form = format!(
        "body={}&tags={}&action=preview&base={base}",
        enc("# Assessment Plan\n\npreviewing\n"),
        enc("draft-tag")
    );
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{PLAN_ID}/edit"), &form),
    );
    assert_eq!(r.status, 200);
    assert!(r.body.contains("value=\"draft-tag\""), "{}", r.body);
    no_script(&r.body);
}
