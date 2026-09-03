// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Integration tests for P4-attach: uploading a file to a page and serving it
//! back, driven through `handle()` over a scratch copy of the fixture wiki.
//!
//! The multipart bodies are built by hand rather than by a shared helper the
//! server also uses. The parser is hand-rolled, so a helper shared with it
//! would drift with it and stop testing the wire format.
//!
//! Two properties carry most of the weight here and are worth naming:
//!
//! * **The content type comes from the extension, never from the upload.** A
//!   browser (or an attacker) may declare anything in the part's own
//!   `Content-Type`; the server ignores it. So a payload's declared type can
//!   never make it executable.
//! * **`svg` is not on the allowlist and that is not an oversight.** Served as
//!   `image/svg+xml` an SVG executes any `<script>` inside it, and the
//!   script-free sweeps cannot see into a bytes response. Refusing the
//!   extension is the gate.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use berrywiki_serve::{handle, App, Request, Response};
use berrywiki_store::LocalFolderStore;

const HOME_ID: &str = "0195f6d0-0000-7000-8000-000000000001";
const TEACHING_ID: &str = "0195f6d0-0000-7000-8000-000000000002";

/// Mirrors `attach::MAX_ATTACHMENT`, written out rather than imported: the
/// constant is crate-private, and a test that read the real value could not
/// notice the value changing.
const MAX_ATTACHMENT: usize = 1024 * 1024;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Copy the fixture wiki's pages into a fresh scratch directory. Only files
/// are copied, so no `assets/` tree arrives with them and every test that
/// needs an attachment uploads its own.
fn scratch_wiki() -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-wiki")
        .canonicalize()
        .expect("fixture exists");
    let dir = std::env::temp_dir().join(format!(
        "berrywiki-attach-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    for entry in fs::read_dir(&fixture).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            fs::copy(&path, dir.join(path.file_name().unwrap())).unwrap();
        }
    }
    dir
}

fn app(wiki: &PathBuf) -> App {
    App::new(LocalFolderStore::open(wiki).unwrap())
}

/// A 1x1 RGBA PNG. Small, but a real image whose bytes must come back
/// unchanged, so a parser that trims one byte too many is visible.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

const BOUNDARY: &str = "----berrywikitestboundary";

/// One file part, with a declared part type the server must ignore.
fn multipart_as(filename: &str, declared: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {declared}\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

fn upload_as(app: &mut App, page: &str, filename: &str, declared: &str, bytes: &[u8]) -> Response {
    let (content_type, body) = multipart_as(filename, declared, bytes);
    handle(
        app,
        &Request::post_bytes(&format!("/page/{page}/attach"), &content_type, body),
    )
}

fn upload(app: &mut App, page: &str, filename: &str, bytes: &[u8]) -> Response {
    upload_as(app, page, filename, "application/octet-stream", bytes)
}

fn get(app: &mut App, target: &str) -> Response {
    handle(app, &Request::get(target))
}

fn no_script(html: &str) {
    let lower = html.to_lowercase();
    assert!(!lower.contains("<script"), "no script element");
    assert!(!lower.contains("javascript:"), "no javascript: URLs");
    assert!(!lower.contains(" onerror="), "no inline handlers");
    assert!(!lower.contains(" onclick="), "no inline handlers");
}

// ---------------------------------------------------------------- round trip

#[test]
fn an_upload_round_trips_byte_for_byte_under_the_type_of_its_extension() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);

    let posted = upload(&mut app, HOME_ID, "berry.png", PNG);
    assert_eq!(posted.status, 303, "a successful upload redirects");
    // Back to the page. On a plain folder there is no commit to report, so the
    // target is bare; the synced backend appends its own notice through the
    // same helper, which `tests/sync.rs` covers.
    assert_eq!(
        posted.location.as_deref(),
        Some(&format!("/page/{HOME_ID}")[..])
    );

    let got = get(&mut app, &format!("/assets/{HOME_ID}/berry.png"));
    assert_eq!(got.status, 200);
    assert_eq!(
        got.bytes.as_deref(),
        Some(PNG),
        "the payload is returned unchanged, not re-encoded or trimmed"
    );
    assert_eq!(got.content_type, "image/png");

    // The file lands in the working tree where a plain `git clone` finds it,
    // which is the whole point: the wiki stays usable without BerryWiki.
    let on_disk = wiki.join("assets").join(HOME_ID).join("berry.png");
    assert_eq!(fs::read(&on_disk).unwrap(), PNG, "{}", on_disk.display());
}

#[test]
fn the_declared_part_type_is_ignored_in_favour_of_the_extension() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);

    // A hostile client declares the payload as SVG, which executes script when
    // a browser is told to treat it as one. The extension is `.png`, so that
    // is what it is served as, and the declaration changes nothing.
    assert_eq!(
        upload_as(&mut app, HOME_ID, "berry.png", "image/svg+xml", PNG).status,
        303
    );
    let got = get(&mut app, &format!("/assets/{HOME_ID}/berry.png"));
    assert_eq!(got.content_type, "image/png");
}

#[test]
fn an_attachment_is_listed_on_the_page_and_on_the_form() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    assert_eq!(upload(&mut app, HOME_ID, "berry.png", PNG).status, 303);

    let href = format!("href=\"/assets/{HOME_ID}/berry.png\"");
    for target in [
        format!("/page/{HOME_ID}"),
        format!("/page/{HOME_ID}/attach"),
    ] {
        let r = get(&mut app, &target);
        assert_eq!(r.status, 200, "{target}");
        assert!(r.body.contains(&href), "{target} links the attachment");
        no_script(&r.body);
    }

    // A page with no attachments says nothing rather than showing an empty
    // list, so the section is evidence that a file is there.
    let other = get(&mut app, &format!("/page/{TEACHING_ID}"));
    assert!(!other.body.contains("Attachments</h2>"), "no empty section");
}

#[test]
fn a_filename_needing_encoding_produces_a_link_that_resolves() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    // `#` would otherwise start a fragment and the space would end the
    // reference, so an HTML-escaped-but-unencoded href is a broken link even
    // though it is well-formed markup.
    assert_eq!(upload(&mut app, HOME_ID, "week 1 #2.png", PNG).status, 303);

    let page = get(&mut app, &format!("/page/{HOME_ID}"));
    let href = format!("href=\"/assets/{HOME_ID}/week%201%20%232.png\"");
    assert!(page.body.contains(&href), "encoded href: {}", page.body);
    // The visible text stays readable rather than showing the encoding.
    assert!(page.body.contains(">week 1 #2.png</a>"));

    // And the link the page just emitted is one the server actually serves.
    let got = get(&mut app, &format!("/assets/{HOME_ID}/week%201%20%232.png"));
    assert_eq!(got.status, 200);
    assert_eq!(got.bytes.as_deref(), Some(PNG));
}

// -------------------------------------------------------------------- refusal

#[test]
fn an_svg_is_refused_because_the_script_sweeps_cannot_see_into_it() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;

    let r = upload(&mut app, HOME_ID, "diagram.svg", svg);
    assert_eq!(r.status, 400, "svg is not an allowed kind of file");
    no_script(&r.body);

    // Refused means not stored, not merely not linked.
    assert!(!wiki
        .join("assets")
        .join(HOME_ID)
        .join("diagram.svg")
        .exists());
    assert_eq!(
        get(&mut app, &format!("/assets/{HOME_ID}/diagram.svg")).status,
        404
    );
}

#[test]
fn extensions_outside_the_allowlist_are_refused_whatever_they_contain() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    // Each of these is either executable in a browser (`html`, `htm`, `js`,
    // `xml`, `svg`) or executable on a machine (`exe`, `sh`). None is served,
    // so none is stored.
    for name in [
        "page.html",
        "page.htm",
        "app.js",
        "data.xml",
        "logo.svg",
        "setup.exe",
        "run.sh",
        "noextension",
    ] {
        let r = upload(&mut app, HOME_ID, name, PNG);
        assert_eq!(r.status, 400, "{name} must be refused");
        assert!(r.body.contains("not an allowed kind of file"), "{name}");
        no_script(&r.body);
    }
    assert!(
        !wiki.join("assets").join(HOME_ID).exists(),
        "nothing was stored at all"
    );
}

#[test]
fn the_extension_match_is_case_insensitive_so_upper_case_cannot_slip_through() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    // `.PNG` is allowed and normalises to the same served type…
    assert_eq!(upload(&mut app, HOME_ID, "berry.PNG", PNG).status, 303);
    assert_eq!(
        get(&mut app, &format!("/assets/{HOME_ID}/berry.PNG")).content_type,
        "image/png"
    );
    // …and `.SVG` is refused exactly as `.svg` is, which is the direction that
    // matters: a case-sensitive table would have let this one through.
    assert_eq!(upload(&mut app, HOME_ID, "d.SVG", PNG).status, 400);
    assert_eq!(upload(&mut app, HOME_ID, "d.HTML", PNG).status, 400);
}

#[test]
fn a_traversal_filename_cannot_escape_the_page_folder() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    // The multipart parser takes the basename after the last separator, which
    // is what browsers on Windows historically sent, so these arrive as plain
    // names rather than as traversals. The assertion is on the outcome: the
    // file lands under this page's folder or nowhere.
    for (name, basename) in [
        ("../../one.png", "one.png"),
        ("..\\..\\two.png", "two.png"),
        ("/etc/passwd.png", "passwd.png"),
        ("../three.png", "three.png"),
    ] {
        let r = upload(&mut app, HOME_ID, name, PNG);
        assert_eq!(r.status, 303, "{name} is taken as its basename");
        assert!(
            wiki.join("assets").join(HOME_ID).join(basename).exists(),
            "{name} landed as {basename}"
        );
        assert!(
            !wiki.join(basename).exists() && !wiki.parent().unwrap().join(basename).exists(),
            "{name} wrote nothing outside the assets folder"
        );
    }
}

#[test]
fn both_separator_styles_yield_the_same_basename() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    // Windows browsers historically sent a full backslash path. If only `/`
    // were stripped, `..\..\a.png` would reach the store as a name containing
    // backslashes, which `validate_component` forbids — so the two styles
    // taking different routes would be visible as a 400 here rather than the
    // 409 that proves they resolved to one and the same name.
    assert_eq!(upload(&mut app, HOME_ID, "../../a.png", PNG).status, 303);
    assert_eq!(upload(&mut app, HOME_ID, "..\\..\\a.png", PNG).status, 409);
}

#[test]
fn a_filename_with_a_forbidden_character_is_refused_not_escaped() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    // `<` and `>` are forbidden in a stored name, so this never becomes a
    // question of escaping it on the way out.
    let r = upload(&mut app, HOME_ID, "<script>.png", PNG);
    assert_eq!(r.status, 400);
    no_script(&r.body);
    assert!(!wiki.join("assets").join(HOME_ID).exists());
}

#[test]
fn a_duplicate_filename_is_refused_rather_than_silently_overwriting() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    assert_eq!(upload(&mut app, HOME_ID, "berry.png", PNG).status, 303);

    let second = upload(&mut app, HOME_ID, "berry.png", b"not a png at all");
    assert_eq!(second.status, 409);
    no_script(&second.body);

    // The original survives, which is the property that matters: never
    // discard content the user already has.
    assert_eq!(
        get(&mut app, &format!("/assets/{HOME_ID}/berry.png"))
            .bytes
            .as_deref(),
        Some(PNG)
    );
}

// ----------------------------------------------------------------------- caps

#[test]
fn a_file_at_the_limit_is_accepted_and_one_byte_over_is_refused() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);

    // Exactly at the limit. This is the case that a single shared cap on the
    // whole request body would have wrongly refused, because the multipart
    // envelope pushes the request past the file's own size.
    let at = vec![b'a'; MAX_ATTACHMENT];
    assert_eq!(upload(&mut app, HOME_ID, "big.txt", &at).status, 303);
    assert_eq!(
        get(&mut app, &format!("/assets/{HOME_ID}/big.txt"))
            .bytes
            .map(|b| b.len()),
        Some(MAX_ATTACHMENT)
    );

    // One byte over. This is the file-sized cap firing, not the request-sized
    // one, and it names the file in the refusal.
    let over = vec![b'a'; MAX_ATTACHMENT + 1];
    let r = upload(&mut app, HOME_ID, "toobig.txt", &over);
    assert_eq!(r.status, 413);
    assert!(r.body.contains("toobig.txt"), "the refusal names the file");
    no_script(&r.body);
    assert!(!wiki
        .join("assets")
        .join(HOME_ID)
        .join("toobig.txt")
        .exists());
}

#[test]
fn an_enormous_request_is_refused_before_it_is_parsed() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    // Well past the envelope allowance, and deliberately not a valid multipart
    // body: reaching a 413 rather than a "not a file upload" 400 is what shows
    // the size check ran first.
    let r = handle(
        &mut app,
        &Request::post_bytes(
            &format!("/page/{HOME_ID}/attach"),
            "multipart/form-data; boundary=x",
            vec![b'a'; MAX_ATTACHMENT + 64 * 1024],
        ),
    );
    assert_eq!(r.status, 413);
    no_script(&r.body);
}

// ------------------------------------------------------------ malformed input

#[test]
fn a_body_that_is_not_an_upload_is_refused_with_a_readable_page() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);

    // A plain form post to the upload route.
    let r = handle(
        &mut app,
        &Request::post(&format!("/page/{HOME_ID}/attach"), "file=berry.png"),
    );
    assert_eq!(r.status, 400);
    assert!(r.body.contains("not a file upload"));
    no_script(&r.body);

    // Multipart, but with no file part in it.
    let body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhi\r\n--{BOUNDARY}--\r\n"
    );
    let r = handle(
        &mut app,
        &Request::post_bytes(
            &format!("/page/{HOME_ID}/attach"),
            &format!("multipart/form-data; boundary={BOUNDARY}"),
            body.into_bytes(),
        ),
    );
    assert_eq!(r.status, 400);
    no_script(&r.body);

    // A file part with an empty filename, which is what an untouched file
    // input submits.
    let r = upload(&mut app, HOME_ID, "", PNG);
    assert_eq!(r.status, 400);
    no_script(&r.body);

    assert!(!wiki.join("assets").exists(), "nothing was stored");
}

#[test]
fn attaching_to_a_page_that_does_not_exist_is_a_not_found_page() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    let r = upload(&mut app, "no-such-page", "berry.png", PNG);
    assert_eq!(r.status, 404);
    no_script(&r.body);
    assert!(!wiki.join("assets").exists());
}

// ---------------------------------------------------------------- asset route

#[test]
fn the_asset_route_answers_one_undifferentiated_not_found() {
    let wiki = scratch_wiki();
    let mut app = app(&wiki);
    assert_eq!(upload(&mut app, HOME_ID, "berry.png", PNG).status, 303);

    // A missing file, a missing page, a disallowed extension and a malformed
    // path must be indistinguishable, or the route becomes a way to ask which
    // pages and files exist.
    let mut bodies = Vec::new();
    for target in [
        format!("/assets/{HOME_ID}/absent.png"),
        format!("/assets/{TEACHING_ID}/berry.png"),
        format!("/assets/{HOME_ID}/berry.svg"),
        "/assets/no-such-page/berry.png".to_string(),
        "/assets/berry.png".to_string(),
        "/assets/".to_string(),
        format!("/assets/{HOME_ID}/../../Home.md"),
        format!("/assets/{HOME_ID}/%2e%2e%2fHome.md"),
    ] {
        let r = get(&mut app, &target);
        assert_eq!(r.status, 404, "{target}");
        assert!(r.bytes.is_none(), "{target} answers with a page, not bytes");
        no_script(&r.body);
        bodies.push(r.body);
    }
    assert!(
        bodies.windows(2).all(|w| w[0] == w[1]),
        "every refusal reads the same"
    );

    // The file that does exist is still served, so the sweep above is not
    // passing because the route is broken.
    assert_eq!(
        get(&mut app, &format!("/assets/{HOME_ID}/berry.png")).status,
        200
    );
}
