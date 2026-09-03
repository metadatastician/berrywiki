// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Attachments: upload, listing and serving (P4-attach, ADR-0011).
//!
//! The engine half of attachments already lived in `berrywiki-store` and
//! `berrywiki-sync`; what was missing was a way to get bytes in and out over
//! HTTP. That is this module, and it is deliberately small and hand-rolled:
//! a `multipart/form-data` parser with no dependency, a fixed extension to
//! content-type table, and two routes.
//!
//! Three rules shape everything here.
//!
//! 1. **Content type comes from the filename extension, never from the
//!    upload.** A browser's declared type is attacker-controlled and sniffing
//!    is worse. The allowlist is the whole authorisation: an extension that is
//!    not in it cannot be stored.
//! 2. **`svg`, `html`, `js` and friends are excluded on purpose.** An SVG
//!    served as `image/svg+xml` executes script inside it, so it is an XSS
//!    vector wearing an image's clothes. This is why the asset routes cannot
//!    be gated by the usual `no_script(&body)` sweep: a binary response has an
//!    empty `body`, so that sweep passes without looking at anything. The
//!    real gate is content-type discipline, tested separately.
//! 3. **Names are validated by the store, not here.** `validate_component`
//!    already rejects traversal, control characters, reserved device names and
//!    the rest. Serve reduces a submitted filename to its basename and hands
//!    the result over; it does not invent a second set of rules that could
//!    drift from the first.

use berrywiki_store::paths::validate_component;
use berrywiki_store::{StoreError, WikiStore};

use crate::editor::with_notice;
use crate::{escape_attr, escape_html, layout, not_found_page, App, Ctx, Response};

/// The largest file that may be attached.
///
/// Deliberately well under the connection-level `MAX_BODY` so that the refusal
/// is a readable page from `handle()` rather than a bare 413 from the socket
/// reader, and so the multipart envelope around the file still fits.
pub(crate) const MAX_ATTACHMENT: usize = 1024 * 1024;

/// Headroom for the multipart wrapper around a maximum-sized file: the
/// boundary, the part headers, and the closing delimiter. Generous, because
/// its only job is to stop the whole-body cap from shadowing the file cap.
pub(crate) const MAX_ENVELOPE: usize = 8 * 1024;

/// Extension to served content type.
///
/// Anything absent from this table cannot be uploaded. The types are all inert
/// when rendered by a browser: no `image/svg+xml`, no `text/html`, no
/// JavaScript, no XML. `.md` is served as plain text rather than
/// `text/markdown` because plain text is the one reading a browser can never
/// turn into an active document.
const ALLOWED: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("pdf", "application/pdf"),
    ("txt", "text/plain; charset=utf-8"),
    ("csv", "text/csv; charset=utf-8"),
    ("md", "text/plain; charset=utf-8"),
];

/// The content type for a filename, or `None` when its extension is not
/// allowed. Extension matching is case-insensitive: `PHOTO.PNG` is a PNG.
pub(crate) fn content_type_for(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit_once('.')?.1.to_ascii_lowercase();
    ALLOWED.iter().find(|(e, _)| *e == ext).map(|(_, ct)| *ct)
}

/// The allowed extensions, for the form's own help text. Kept derived from
/// `ALLOWED` so the page can never advertise something the code refuses.
fn allowed_list() -> String {
    let names: Vec<String> = ALLOWED.iter().map(|(e, _)| format!(".{e}")).collect();
    names.join(", ")
}

// --- multipart -------------------------------------------------------------

/// One `multipart/form-data` part that carried a filename.
pub(crate) struct FilePart {
    pub(crate) filename: String,
    pub(crate) bytes: Vec<u8>,
}

/// Why an upload could not be read. Each maps to a specific, readable page;
/// none of them is a 500.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MultipartError {
    /// The request was not `multipart/form-data`, or carried no boundary.
    NotMultipart,
    /// No part in the body carried a `filename`.
    NoFile,
    /// More than one file part. Refused rather than silently taking the first.
    ManyFiles,
    /// A file part was present but its filename was empty, which is what a
    /// browser sends when the user submits the form having chosen nothing.
    EmptyFilename,
}

/// The boundary token from a `Content-Type` header, if this is a multipart
/// form. The value may be quoted; both spellings are accepted.
fn boundary_of(content_type: &str) -> Option<String> {
    let (kind, params) = content_type.split_once(';')?;
    if !kind.trim().eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    for param in params.split(';') {
        let (k, v) = match param.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        if k.trim().eq_ignore_ascii_case("boundary") {
            let v = v.trim().trim_matches('"');
            if v.is_empty() {
                return None;
            }
            return Some(v.to_string());
        }
    }
    None
}

/// Index of `needle` in `hay` at or after `from`.
///
/// `str::find` is not available here because the body is bytes, not text, and
/// must stay that way: an upload is not required to be UTF-8.
fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// Strip one leading and one trailing CRLF, which the delimiter grammar puts
/// around every part's content.
fn trim_crlf(mut part: &[u8]) -> &[u8] {
    if part.starts_with(b"\r\n") {
        part = &part[2..];
    }
    if part.ends_with(b"\r\n") {
        part = &part[..part.len() - 2];
    }
    part
}

/// The `filename` parameter of a `Content-Disposition` header, reduced to its
/// basename.
///
/// Some browsers have historically sent a full client-side path. Everything up
/// to the last `/` or `\` is discarded before the name goes anywhere near the
/// store, so a path that arrives is a name, not a location.
fn filename_of(headers: &str) -> Option<String> {
    for line in headers.split("\r\n") {
        let (k, v) = line.split_once(':')?;
        if !k.trim().eq_ignore_ascii_case("content-disposition") {
            continue;
        }
        for param in v.split(';') {
            let (pk, pv) = match param.split_once('=') {
                Some(kv) => kv,
                None => continue,
            };
            if pk.trim().eq_ignore_ascii_case("filename") {
                let raw = pv.trim().trim_matches('"');
                let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
                return Some(base.to_string());
            }
        }
    }
    None
}

/// Parse an upload, returning the single file part.
///
/// Exactly one file is expected. Zero and many are both errors rather than a
/// best guess, because "which of your two files did it keep" is not a question
/// a wiki should ever make its user ask.
pub(crate) fn parse_upload(content_type: &str, body: &[u8]) -> Result<FilePart, MultipartError> {
    let boundary = boundary_of(content_type).ok_or(MultipartError::NotMultipart)?;
    let delimiter = format!("--{boundary}").into_bytes();

    let mut files: Vec<FilePart> = Vec::new();
    let mut saw_empty_filename = false;
    let mut cursor = match find_from(body, &delimiter, 0) {
        Some(i) => i + delimiter.len(),
        None => return Err(MultipartError::NotMultipart),
    };

    while let Some(next) = find_from(body, &delimiter, cursor) {
        let part = trim_crlf(&body[cursor..next]);
        cursor = next + delimiter.len();

        // Headers and content are separated by a blank line. A part without
        // one is malformed and is skipped rather than guessed at.
        let Some(split) = find_from(part, b"\r\n\r\n", 0) else {
            continue;
        };
        let headers = String::from_utf8_lossy(&part[..split]).into_owned();
        let content = &part[split + 4..];

        // A part with no filename is an ordinary form field, not a file.
        let Some(name) = filename_of(&headers) else {
            continue;
        };
        if name.is_empty() {
            saw_empty_filename = true;
            continue;
        }
        files.push(FilePart {
            filename: name,
            bytes: content.to_vec(),
        });
    }

    if files.len() > 1 {
        return Err(MultipartError::ManyFiles);
    }
    match files.pop() {
        Some(f) => Ok(f),
        None if saw_empty_filename => Err(MultipartError::EmptyFilename),
        None => Err(MultipartError::NoFile),
    }
}

// --- views and handlers ----------------------------------------------------

/// The upload form, plus what is already attached.
pub(crate) fn attach_form(ctx: Ctx<'_>, id: &str, error: Option<(u16, String)>) -> Response {
    let Ok(page) = ctx.store.read_page(id) else {
        return not_found_page(ctx, id);
    };
    let id_a = escape_attr(id);
    let mut main = String::new();
    let status = match &error {
        Some((s, e)) => {
            main.push_str(&format!("<p class=\"error-banner\">{}</p>", escape_html(e)));
            *s
        }
        None => 200,
    };

    main.push_str(&format!(
        "<p class=\"page-actions\"><a href=\"/page/{id_a}\">Back to the page</a></p>"
    ));
    main.push_str(&format!(
        "<p class=\"hint\">Files are stored in <code>assets/{}/</code> in the wiki \
         folder, so they travel with the repository and stay reachable from a plain \
         clone. Link to one from the page body with \
         <code>![alt](assets/{}/name.png)</code>.</p>",
        escape_html(id),
        escape_html(id),
    ));

    // The form is `multipart/form-data` because that is the only encoding a
    // browser will use for a file, and it is submitted without any script.
    main.push_str(&format!(
        "<form method=\"post\" action=\"/page/{id_a}/attach\" \
         enctype=\"multipart/form-data\">\
         <p><label for=\"attach-file\">File</label><br>\
         <input type=\"file\" id=\"attach-file\" name=\"file\" required></p>\
         <div class=\"editor-buttons\"><button type=\"submit\">Attach</button></div>\
         </form>"
    ));
    main.push_str(&format!(
        "<p class=\"hint\">At most {} KiB. Allowed: {}. Types that a browser can \
         execute — SVG, HTML, JavaScript — are refused, because an attachment is \
         served from this wiki's own origin.</p>",
        MAX_ATTACHMENT / 1024,
        escape_html(&allowed_list()),
    ));

    main.push_str(&attachment_list(ctx, id));

    let title = format!("Attach to {}", page.title);
    Response::html(status, layout(ctx, Some(id), &title, main))
}

/// The `<ul>` of a page's attachments, or a short empty-state.
///
/// Shared by the form and the page footer so a reader and an author see the
/// same list from the same source.
pub(crate) fn attachment_list(ctx: Ctx<'_>, id: &str) -> String {
    let items = match ctx.store.attachments(id) {
        Ok(a) => a,
        // A listing failure is reported, not hidden: a page that silently
        // claims to have no files when it has some is worse than an error.
        Err(e) => {
            return format!(
                "<p class=\"error-banner\">Could not list attachments: {}</p>",
                escape_html(&e.to_string())
            )
        }
    };
    if items.is_empty() {
        return String::new();
    }
    let mut out = String::from("<section class=\"attachments\"><h2>Attachments</h2><ul>");
    for a in &items {
        // The href is percent-encoded, not merely HTML-escaped. Escaping alone
        // makes the attribute well-formed whilst leaving the URL wrong: a
        // filename containing `#` would be read as a fragment and a space
        // would end the reference. `percent_encode` emits only unreserved
        // characters plus `%`, so its output needs no further attribute
        // escaping; the visible text is escaped separately because it is
        // markup, not a URL.
        out.push_str(&format!(
            "<li><a href=\"/assets/{}/{}\">{}</a></li>",
            percent_path(&a.page_id),
            percent_path(&a.filename),
            escape_html(&a.filename),
        ));
    }
    out.push_str("</ul></section>");
    out
}

/// `POST /page/<id>/attach`.
///
/// Takes the whole `Request` rather than a body string: an upload is bytes and
/// the boundary lives in a header, neither of which the form-post path carries.
pub(crate) fn post_attach(app: &mut App, id: &str, req: &crate::Request) -> Response {
    if app.store().read_page(id).is_err() {
        return not_found_page(app.ctx(), id);
    }
    // Two caps, and the order matters. This one bounds the whole request so an
    // enormous body is refused before it is parsed; the second bounds the file
    // itself. They must not be the same number: a multipart body is always
    // larger than the file inside it, so capping both at `MAX_ATTACHMENT`
    // would make the second check unreachable — a gate that cannot fire. The
    // envelope allowance is what keeps the file-sized check operative, and a
    // file of exactly the limit still gets through.
    if req.bytes.len() > MAX_ATTACHMENT + MAX_ENVELOPE {
        return attach_form(
            app.ctx(),
            id,
            Some((
                413,
                format!(
                    "That upload is larger than the {} KiB limit.",
                    MAX_ATTACHMENT / 1024
                ),
            )),
        );
    }

    let part = match parse_upload(&req.content_type, &req.bytes) {
        Ok(p) => p,
        Err(e) => {
            let msg = match e {
                MultipartError::NotMultipart => {
                    "That request was not a file upload. Use the form on this page."
                }
                MultipartError::NoFile => "No file was included in the upload.",
                MultipartError::ManyFiles => "Attach one file at a time.",
                MultipartError::EmptyFilename => "No file was chosen.",
            };
            return attach_form(app.ctx(), id, Some((400, msg.to_string())));
        }
    };

    if part.bytes.len() > MAX_ATTACHMENT {
        return attach_form(
            app.ctx(),
            id,
            Some((
                413,
                format!(
                    "“{}” is larger than the {} KiB limit.",
                    part.filename,
                    MAX_ATTACHMENT / 1024
                ),
            )),
        );
    }
    if content_type_for(&part.filename).is_none() {
        return attach_form(
            app.ctx(),
            id,
            Some((
                400,
                format!(
                    "“{}” is not an allowed kind of file. Allowed: {}.",
                    part.filename,
                    allowed_list()
                ),
            )),
        );
    }
    // The name is checked here only to produce a readable message; the store
    // checks it again and is the authority.
    if validate_component(&part.filename).is_err() {
        return attach_form(
            app.ctx(),
            id,
            Some((
                400,
                format!("“{}” is not a usable filename.", part.filename),
            )),
        );
    }

    match app.add_attachment(id, &part.filename, &part.bytes) {
        Ok(rec) => Response::see_other(with_notice(&format!("/page/{}", percent_path(id)), rec)),
        Err(e) => {
            let status = if matches!(
                e,
                berrywiki_sync::SyncError::Store(StoreError::DuplicateAttachment { .. })
            ) {
                409
            } else {
                400
            };
            attach_form(app.ctx(), id, Some((status, e.to_string())))
        }
    }
}

/// `GET /assets/<page-id>/<filename>`.
///
/// Read-only, so it lives in `dispatch` and works on the `--github` mirror path
/// as well as the editor. The content type comes from the extension table and
/// from nowhere else; a file whose extension is not allowed is a 404 even if it
/// somehow reached the folder, because serving it is the whole risk.
pub(crate) fn asset(ctx: Ctx<'_>, rest: &str) -> Response {
    let Some((page_id, filename)) = rest.split_once('/') else {
        return asset_missing(ctx);
    };
    let page_id = crate::percent_decode(page_id);
    let filename = crate::percent_decode(filename);
    let Some(ct) = content_type_for(&filename) else {
        return asset_missing(ctx);
    };
    match ctx.store.read_attachment(&page_id, &filename) {
        Ok(bytes) => Response::binary(ct, bytes),
        Err(_) => asset_missing(ctx),
    }
}

/// One shape of not-found for every asset failure.
///
/// Deliberately undifferentiated: a missing page, a missing file, a refused
/// extension and a rejected name all read the same, so the route cannot be
/// used to probe what exists in the folder.
fn asset_missing(ctx: Ctx<'_>) -> Response {
    Response::html(
        404,
        layout(ctx, None, "Not found", "<p>No such file.</p>".to_string()),
    )
}

/// Percent-encode a page id for use in a redirect target.
fn percent_path(id: &str) -> String {
    crate::percent_encode(id)
}
