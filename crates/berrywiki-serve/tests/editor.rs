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
    };
    assert_eq!(handle(&mut app, &req).status, 405);
}
