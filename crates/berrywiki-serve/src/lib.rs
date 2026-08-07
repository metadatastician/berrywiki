//! Zero-JavaScript, server-side-rendered explorer and editor for BerryWiki
//! (ADR-0005, P2-edit).
//!
//! The routing logic is pure and socket-free: [`handle`] maps a [`Request`]
//! to a [`Response`] against an [`App`] (store + optional draft store), so
//! the whole UI is testable in-process with no sockets. [`route`] remains the
//! read-only GET core used by the `--github` mirror path. [`serve`] /
//! [`serve_readonly`] are thin blocking accept loops over `std::net`.
//!
//! Invariants:
//! * **No `<script>` ever ships.** Every response is HTML with inline CSS and
//!   plain forms/links; a test asserts no script element in any route.
//! * All dynamic text is HTML-escaped at the boundary; page *body* content is
//!   rendered by `berrywiki-render`, which escapes raw HTML and neutralises
//!   dangerous URL schemes.
//! * An error response never discards submitted text (see `editor`).

use std::collections::HashSet;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use berrywiki_draft::DraftStore;
use berrywiki_render::render_markdown;
use berrywiki_store::{LocalFolderStore, WikiStore};

mod editor;
mod ids;

/// A minimal HTTP response.
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
    /// `Location` header for 303 redirects (Post/Redirect/Get).
    pub location: Option<String>,
}

impl Response {
    fn html(status: u16, body: String) -> Self {
        Response {
            status,
            content_type: "text/html; charset=utf-8",
            body,
            location: None,
        }
    }

    /// A 303 See Other redirect — every successful POST answers with one so a
    /// browser refresh can never resubmit the form.
    fn see_other(to: impl Into<String>) -> Self {
        Response {
            status: 303,
            content_type: "text/html; charset=utf-8",
            body: String::new(),
            location: Some(to.into()),
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

/// A parsed HTTP request, decoupled from the socket so tests can construct it.
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    /// Raw `application/x-www-form-urlencoded` body (empty for GET).
    pub body: String,
}

impl Request {
    /// Convenience constructor for a GET request.
    pub fn get(target: &str) -> Self {
        let (path, query) = split_target(target);
        Request {
            method: "GET".to_string(),
            path,
            query,
            body: String::new(),
        }
    }

    /// Convenience constructor for a form POST.
    pub fn post(target: &str, body: &str) -> Self {
        let (path, query) = split_target(target);
        Request {
            method: "POST".to_string(),
            path,
            query,
            body: body.to_string(),
        }
    }
}

fn split_target(target: &str) -> (String, String) {
    match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    }
}

/// The served application: the store plus the editing facilities.
///
/// Drafts live **outside** the wiki clone (ADR-0006/0008); when the app-state
/// home cannot be resolved the editor still works but Save-draft is visibly
/// unavailable — degrade, never panic.
pub struct App {
    pub(crate) store: LocalFolderStore,
    pub(crate) drafts: Option<DraftStore>,
}

impl App {
    /// Wire the draft store through the store's app-state home
    /// (`$XDG_STATE_HOME/berrywiki/<repo-id>/drafts/`).
    pub fn new(store: LocalFolderStore) -> Self {
        let drafts = store.appstate().map(|a| DraftStore::new(a.drafts_dir()));
        App { store, drafts }
    }

    /// Explicit draft store (or `None` for the degraded mode) — used by tests
    /// so they never depend on environment variables.
    pub fn with_drafts(store: LocalFolderStore, drafts: Option<DraftStore>) -> Self {
        App { store, drafts }
    }

    pub fn store(&self) -> &LocalFolderStore {
        &self.store
    }

    pub(crate) fn ctx(&self) -> Ctx<'_> {
        Ctx {
            store: &self.store,
            drafts: self.drafts.as_ref(),
            editing: true,
        }
    }
}

/// Rendering context: what the view functions may consult. `editing: false`
/// (the `route()` / `--github` path) renders no edit affordances at all.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    pub(crate) store: &'a LocalFolderStore,
    pub(crate) drafts: Option<&'a DraftStore>,
    pub(crate) editing: bool,
}

/// Handle a request against an editable app. Pure and socket-free — the unit
/// of test for the editor.
pub fn handle(app: &mut App, req: &Request) -> Response {
    match req.method.as_str() {
        "GET" => handle_get(app, &req.path, &req.query),
        "POST" => editor::handle_post(app, &req.path, &req.body),
        _ => Response::html(405, "<h1>405 Method Not Allowed</h1>".to_string()),
    }
}

fn handle_get(app: &App, path: &str, query: &str) -> Response {
    let ctx = app.ctx();
    if let Some(rest) = path.strip_prefix("/page/") {
        if let Some(id) = rest.strip_suffix("/edit") {
            return editor::edit_form(ctx, &percent_decode(id), query);
        }
        if let Some(id) = rest.strip_suffix("/delete") {
            return editor::delete_confirm(ctx, &percent_decode(id), None);
        }
    }
    if path == "/new" {
        return editor::new_form(ctx, query);
    }
    dispatch(ctx, path, query)
}

/// Route a GET request read-only. Pure and socket-free; kept as the public
/// core for the `--github` mirror path and the read-only tests.
pub fn route(store: &LocalFolderStore, path: &str, query: &str) -> Response {
    dispatch(
        Ctx {
            store,
            drafts: None,
            editing: false,
        },
        path,
        query,
    )
}

fn dispatch(ctx: Ctx<'_>, path: &str, query: &str) -> Response {
    if path == "/" {
        return home_page(ctx);
    }
    if path == "/diagnostics" {
        return diagnostics_page(ctx);
    }
    if path == "/search" {
        return search_page(ctx, &query_value(query, "q"));
    }
    if let Some(rest) = path.strip_prefix("/page/") {
        return page_view(ctx, &percent_decode(rest));
    }
    Response::html(
        404,
        layout(ctx, None, "Not found", "<p>No such page.</p>".to_string()),
    )
}

fn home_page(ctx: Ctx<'_>) -> Response {
    // Land on the first root page if there is one, else an empty-state.
    if let Some(root) = ctx.store.graph().roots().first() {
        return page_view(ctx, &root.id);
    }
    let hint = if ctx.editing {
        "<p>This wiki has no pages yet. <a href=\"/new\">Create the first page.</a></p>"
    } else {
        "<p>This wiki has no pages yet.</p>"
    };
    Response::html(200, layout(ctx, None, "BerryWiki", hint.to_string()))
}

pub(crate) fn not_found_page(ctx: Ctx<'_>, id: &str) -> Response {
    Response::html(
        404,
        layout(
            ctx,
            None,
            "Not found",
            format!("<p>No page with id <code>{}</code>.</p>", escape_html(id)),
        ),
    )
}

fn page_view(ctx: Ctx<'_>, id: &str) -> Response {
    let page = match ctx.store.read_page(id) {
        Ok(p) => p,
        Err(_) => return not_found_page(ctx, id),
    };

    let mut main = String::new();
    if ctx.editing {
        let has_draft = ctx.drafts.map(|d| d.has(id)).unwrap_or(false);
        let badge = if has_draft {
            format!(
                " <span class=\"draft-badge\">unsaved draft — \
                 <a href=\"/page/{}/edit\">edit to resume</a></span>",
                escape_attr(id)
            )
        } else {
            String::new()
        };
        main.push_str(&format!(
            "<p class=\"page-actions\"><a href=\"/page/{id_a}/edit\">Edit</a> · \
             <a href=\"/new?parent={id_a}\">New subpage</a> · \
             <a class=\"danger\" href=\"/page/{id_a}/delete\">Delete…</a>{badge}</p>",
            id_a = escape_attr(id),
            badge = badge,
        ));
    }
    let rendered = render_markdown(&page.body);
    main.push_str(&format!("<article class=\"page\">{rendered}</article>"));
    let aside = context_pane(ctx, id);
    let body = layout_three(ctx, Some(id), &page.title, &main, &aside);
    Response::html(200, body)
}

fn diagnostics_page(ctx: Ctx<'_>) -> Response {
    let diags: Vec<String> = ctx
        .store
        .graph()
        .diagnostics()
        .iter()
        .chain(ctx.store.load_diagnostics().iter())
        .map(|d| {
            format!(
                "<li class=\"diag {}\"><code>{}</code> {}</li>",
                d.severity,
                escape_html(&d.code),
                escape_html(&d.message)
            )
        })
        .collect();
    let main = if diags.is_empty() {
        "<p>No diagnostics — the notebook is consistent.</p>".to_string()
    } else {
        format!("<ul class=\"diags\">{}</ul>", diags.join(""))
    };
    Response::html(200, layout(ctx, None, "Diagnostics", main))
}

fn search_page(ctx: Ctx<'_>, q: &str) -> Response {
    let needle = q.trim().to_lowercase();
    let main = if needle.is_empty() {
        "<p>Type a query above.</p>".to_string()
    } else {
        let mut hits = Vec::new();
        for page in ctx.store.graph().pages() {
            let in_title = page.title.to_lowercase().contains(&needle);
            let in_body = page.body.to_lowercase().contains(&needle);
            if in_title || in_body {
                hits.push(format!(
                    "<li><a href=\"/page/{}\">{}</a>{}</li>",
                    escape_attr(&page.id),
                    escape_html(&page.title),
                    if in_title {
                        ""
                    } else {
                        " <small>(body)</small>"
                    }
                ));
            }
        }
        if hits.is_empty() {
            format!("<p>No pages match “{}”.</p>", escape_html(q))
        } else {
            format!(
                "<p>{} result(s) for “{}”:</p><ul class=\"results\">{}</ul>",
                hits.len(),
                escape_html(q),
                hits.join("")
            )
        }
    };
    Response::html(200, layout(ctx, None, "Search", main))
}

/// The right-hand context pane: outline, tags, backlinks.
fn context_pane(ctx: Ctx<'_>, id: &str) -> String {
    let page = match ctx.store.read_page(id) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };

    let mut out = String::new();

    if !page.headings.is_empty() {
        out.push_str("<h2>Outline</h2><ul class=\"outline\">");
        for h in &page.headings {
            // Plain text (no anchor jumping yet — see work package P1 follow-up).
            out.push_str(&format!(
                "<li class=\"h{}\">{}</li>",
                h.depth.min(6),
                escape_html(&h.text)
            ));
        }
        out.push_str("</ul>");
    }

    if let Some(meta) = &page.metadata {
        if !meta.tags.is_empty() {
            out.push_str("<h2>Tags</h2><p class=\"tags\">");
            for tag in &meta.tags {
                out.push_str(&format!("<span class=\"tag\">{}</span> ", escape_html(tag)));
            }
            out.push_str("</p>");
        }
    }

    let backlinks = ctx.store.graph().backlinks_of(id);
    if !backlinks.is_empty() {
        out.push_str("<h2>Backlinks</h2><ul class=\"backlinks\">");
        for bl in backlinks {
            out.push_str(&format!(
                "<li><a href=\"/page/{}\">{}</a></li>",
                escape_attr(&bl.from_id),
                escape_html(&bl.from_title)
            ));
        }
        out.push_str("</ul>");
    }

    out
}

/// The navigation tree (left pane). Pages with an unsaved draft carry a dot
/// marker — the "visible unsaved state" the spec requires.
fn nav_tree(ctx: Ctx<'_>, current: Option<&str>) -> String {
    let draft_ids: HashSet<String> = ctx
        .drafts
        .map(|d| d.list().into_iter().map(|s| s.page_id).collect())
        .unwrap_or_default();
    let mut out = String::from("<nav class=\"tree\" aria-label=\"Notebook\"><ul>");
    for (depth, page) in ctx.store.graph().walk() {
        let is_current = current == Some(page.id.as_str());
        let archived = if page.is_archived() { " archived" } else { "" };
        let dot = if draft_ids.contains(&page.id) {
            "<span class=\"draft-dot\" title=\"unsaved draft\"></span>"
        } else {
            ""
        };
        out.push_str(&format!(
            "<li style=\"--depth:{depth}\" class=\"tree-item{archived}{}\">\
             <a href=\"/page/{}\"{}>{}{dot}</a></li>",
            if is_current { " current" } else { "" },
            escape_attr(&page.id),
            if is_current {
                " aria-current=\"page\""
            } else {
                ""
            },
            escape_html(&page.title),
        ));
    }
    out.push_str("</ul></nav>");
    out
}

/// Two-pane layout (nav + main), used for search/diagnostics/empty.
pub(crate) fn layout(ctx: Ctx<'_>, current: Option<&str>, title: &str, main: String) -> String {
    layout_three(ctx, current, title, &main, "")
}

/// Three-pane layout (nav + main + aside).
pub(crate) fn layout_three(
    ctx: Ctx<'_>,
    current: Option<&str>,
    title: &str,
    main: &str,
    aside: &str,
) -> String {
    let nav = nav_tree(ctx, current);
    let aside_html = if aside.is_empty() {
        String::new()
    } else {
        format!("<aside class=\"context\">{aside}</aside>")
    };
    let new_link = if ctx.editing {
        "<a class=\"new-link\" href=\"/new\">New page</a>"
    } else {
        ""
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title} — BerryWiki</title><style>{CSS}</style></head><body>\
<header class=\"topbar\"><a class=\"brand\" href=\"/\">BerryWiki</a>{new_link}\
<form class=\"search\" method=\"get\" action=\"/search\" role=\"search\">\
<input type=\"search\" name=\"q\" placeholder=\"Search…\" aria-label=\"Search\">\
<button type=\"submit\">Search</button></form>\
<a class=\"diag-link\" href=\"/diagnostics\">Diagnostics</a></header>\
<div class=\"grid\">{nav}<main class=\"main\"><h1>{title_h1}</h1>{main}</main>{aside_html}</div>\
</body></html>",
        title = escape_html(title),
        title_h1 = escape_html(title),
        new_link = new_link,
        nav = nav,
        main = main,
        aside_html = aside_html,
    )
}

const CSS: &str = "\
:root{--depth:0}\
*{box-sizing:border-box}\
body{margin:0;font:15px/1.5 system-ui,sans-serif;color:#1a1a1a;background:#fff}\
.topbar{display:flex;gap:1rem;align-items:center;padding:.6rem 1rem;background:#7a1f2b;color:#fff;position:sticky;top:0}\
.brand{font-weight:700;color:#fff;text-decoration:none;font-size:1.1rem}\
.new-link{color:#fff;text-decoration:none;opacity:.9}\
.search{margin-left:auto;display:flex;gap:.3rem}\
.search input{padding:.3rem .5rem;border:0;border-radius:3px}\
.search button{padding:.3rem .7rem;border:0;border-radius:3px;background:#fff;color:#7a1f2b;cursor:pointer}\
.diag-link{color:#fff;text-decoration:none;opacity:.9}\
.grid{display:grid;grid-template-columns:16rem minmax(0,1fr) 15rem;gap:0;min-height:calc(100vh - 3rem)}\
.tree{border-right:1px solid #e5e5e5;padding:.5rem 0;overflow:auto}\
.tree ul{list-style:none;margin:0;padding:0}\
.tree-item a{display:block;padding:.2rem .6rem .2rem calc(.6rem + var(--depth)*.9rem);color:#333;text-decoration:none;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}\
.tree-item a:hover{background:#f3f3f3}\
.tree-item.current a{background:#f0e0e2;color:#7a1f2b;font-weight:600}\
.tree-item.archived a{opacity:.55;font-style:italic}\
.main{padding:1rem 2rem;min-width:0}\
.main h1{margin-top:0}\
.page table{border-collapse:collapse}\
.page th,.page td{border:1px solid #ccc;padding:.3rem .6rem}\
.page pre{background:#f6f6f6;padding:.6rem;overflow:auto;border-radius:4px}\
.context{border-left:1px solid #e5e5e5;padding:.5rem 1rem;font-size:.9rem}\
.context h2{font-size:.8rem;text-transform:uppercase;letter-spacing:.03em;color:#888;margin:1rem 0 .3rem}\
.outline{list-style:none;padding:0;margin:0}\
.outline .h2{padding-left:.6rem}.outline .h3{padding-left:1.2rem}.outline .h4{padding-left:1.8rem}\
.tag{background:#f0e0e2;color:#7a1f2b;padding:.05rem .4rem;border-radius:3px;font-size:.8rem}\
.diags{list-style:none;padding:0}.diag{padding:.3rem .5rem;border-left:3px solid #ccc;margin:.3rem 0}\
.diag.warning{border-color:#c9a227}.diag.error{border-color:#c0392b}\
.page-actions{margin:.2rem 0 .8rem;font-size:.9rem}\
.danger{color:#c0392b}\
.draft-badge{background:#fdf6e3;color:#7a5b00;padding:.1rem .45rem;border-radius:3px;font-size:.8rem}\
.draft-dot{display:inline-block;width:.45rem;height:.45rem;border-radius:50%;background:#c9a227;margin-left:.35rem;vertical-align:middle}\
.editor{max-width:60rem}\
.editor textarea{width:100%;min-height:22rem;font:13px/1.5 ui-monospace,SFMono-Regular,monospace;padding:.6rem;border:1px solid #ccc;border-radius:4px;resize:vertical}\
.editor-buttons{display:flex;gap:.5rem;margin:.6rem 0}\
.editor-buttons button{padding:.35rem .9rem;border:1px solid #7a1f2b;border-radius:3px;background:#7a1f2b;color:#fff;cursor:pointer}\
.editor-buttons button.secondary{background:#fff;color:#7a1f2b}\
.editor-field{margin:.5rem 0}\
.editor-field label{display:block;font-size:.85rem;color:#555;margin-bottom:.2rem}\
.editor-field input,.editor-field select{padding:.3rem .5rem;border:1px solid #ccc;border-radius:3px;min-width:18rem}\
.notice{background:#e7f4e7;border-left:3px solid #2e7d32;padding:.4rem .6rem;margin:.5rem 0}\
.error-banner{background:#fbeaea;border-left:3px solid #c0392b;padding:.4rem .6rem;margin:.5rem 0}\
.draft-banner{background:#fdf6e3;border-left:3px solid #c9a227;padding:.4rem .6rem;margin:.5rem 0}\
.drafts-unavailable{color:#7a5b00;font-size:.85rem}\
.preview{border-top:1px dashed #bbb;margin-top:1.2rem;padding-top:.6rem}\
.inline-form{display:inline}\
.inline-form button{padding:.2rem .6rem;border:1px solid #7a1f2b;border-radius:3px;background:#fff;color:#7a1f2b;cursor:pointer}\
@media(prefers-color-scheme:dark){body{background:#161616;color:#e6e6e6}.tree,.context{border-color:#333}.tree-item a{color:#cfcfcf}.tree-item a:hover{background:#222}.page pre{background:#222}\
.editor textarea{background:#1d1d1d;color:#e6e6e6;border-color:#444}\
.editor-field input,.editor-field select{background:#1d1d1d;color:#e6e6e6;border-color:#444}\
.notice{background:#12290f}.error-banner{background:#2e1212}.draft-banner{background:#2b230c}}";

// --- helpers ---------------------------------------------------------------

/// HTML-escape text content.
pub fn escape_html(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

/// Escape a value going into a double-quoted attribute (e.g. an href).
pub(crate) fn escape_attr(s: &str) -> String {
    escape_html(s)
}

/// Extract a query-string value by key, percent- and plus-decoding it.
pub(crate) fn query_value(query: &str, key: &str) -> String {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return percent_decode(&v.replace('+', " "));
            }
        }
    }
    String::new()
}

/// Extract a field from an `application/x-www-form-urlencoded` body — the
/// encoding is byte-identical to a query string.
pub(crate) fn form_value(body: &str, key: &str) -> String {
    query_value(body, key)
}

/// Percent-decode (`%XX`) a string. Invalid escapes are left literal.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Normalise CRLF (and stray CR) to LF. Browsers submit textarea content with
/// `\r\n`; without this every Save would rewrite the whole file with CRLF,
/// breaking byte-determinism and polluting diffs.
pub(crate) fn normalize_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// Stable FNV-1a hash of a page's raw source, carried through the edit form as
/// the hidden `base` field. Same algorithm as `berrywiki-appstate::repo_id`
/// (hand-rolled because `DefaultHasher` output is not stable across releases).
pub(crate) fn source_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in s.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

// --- server ----------------------------------------------------------------

/// Maximum accepted POST body (a wiki page is text; 2 MiB is generous).
const MAX_BODY: usize = 2 * 1024 * 1024;

/// Blocking, single-threaded HTTP server for a single-user localhost session,
/// with editing enabled. Returns only on a listener error. Single-threaded on
/// purpose: `&mut App` is race-free in-process.
pub fn serve(app: &mut App, addr: &str) -> io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                // A per-connection error must not bring the server down.
                let _ = handle_connection_app(&mut s, app);
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

/// Blocking read-only server (the `--github` mirror path): GET via [`route`],
/// everything else 405.
pub fn serve_readonly(store: &LocalFolderStore, addr: &str) -> io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let _ = handle_connection_readonly(&mut s, store);
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

fn handle_connection_readonly(stream: &mut TcpStream, store: &LocalFolderStore) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };

    let response = if method == "GET" {
        route(store, path, query)
    } else {
        Response::html(405, "<h1>405 Method Not Allowed</h1>".to_string())
    };

    write_response(stream, &response)
}

fn handle_connection_app(stream: &mut TcpStream, app: &mut App) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();

    // Read headers only for Content-Length; nothing else matters to us.
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }

    if content_length > MAX_BODY {
        return write_response(
            stream,
            &Response::html(413, "<h1>413 Payload Too Large</h1>".to_string()),
        );
    }
    let mut body_bytes = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body_bytes)?;
    }
    // Invalid UTF-8 degrades lossily rather than panicking or dropping the
    // request (spec: malformed input degrades with a diagnostic).
    let body = String::from_utf8_lossy(&body_bytes).into_owned();

    let (path, query) = split_target(&target);
    let req = Request {
        method,
        path,
        query,
        body,
    };
    let response = handle(app, &req);
    write_response(stream, &response)
}

fn write_response(stream: &mut TcpStream, response: &Response) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    if let Some(loc) = &response.location {
        // Belt-and-braces: a redirect target must be a local absolute path and
        // can never smuggle header bytes.
        debug_assert!(loc.starts_with('/') && !loc.contains(['\r', '\n']));
        head.push_str(&format!("Location: {loc}\r\n"));
    }
    head.push_str(&format!(
        "Content-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.content_type,
        response.body.len(),
    ));
    stream.write_all(head.as_bytes())?;
    stream.write_all(response.body.as_bytes())?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const HOME_ID: &str = "0195f6d0-0000-7000-8000-000000000001";
    const PLAN_ID: &str = "0195f6ec-36a2-7a42-b519-5f558842e256";

    fn store() -> LocalFolderStore {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/test-wiki")
            .canonicalize()
            .unwrap();
        LocalFolderStore::open(dir).unwrap()
    }

    fn no_script(html: &str) {
        let lower = html.to_lowercase();
        assert!(!lower.contains("<script"), "no script element");
        assert!(!lower.contains("javascript:"), "no javascript: URLs");
        assert!(!lower.contains(" onerror="), "no inline handlers");
        assert!(!lower.contains(" onclick="), "no inline handlers");
    }

    #[test]
    fn home_renders_first_root_with_no_script() {
        let s = store();
        let r = route(&s, "/", "");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("BerryWiki"));
        assert!(r.body.contains("Home"));
        no_script(&r.body);
    }

    #[test]
    fn page_view_renders_body_tree_and_backlinks() {
        let s = store();
        let r = route(&s, &format!("/page/{PLAN_ID}"), "");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("Assessment Plan"));
        assert!(r.body.contains("<table>"), "GFM table rendered");
        assert!(r.body.contains("Backlinks"), "context pane present");
        // The nav tree links to other pages.
        assert!(r.body.contains(&format!("/page/{HOME_ID}")));
        no_script(&r.body);
    }

    #[test]
    fn readonly_route_shows_no_edit_affordances() {
        let s = store();
        let r = route(&s, &format!("/page/{PLAN_ID}"), "");
        assert!(
            !r.body.contains("/edit"),
            "read-only view must not offer Edit"
        );
        assert!(
            !r.body.contains("/new"),
            "read-only view must not offer New"
        );
    }

    #[test]
    fn unknown_page_is_404() {
        let s = store();
        let r = route(&s, "/page/does-not-exist", "");
        assert_eq!(r.status, 404);
        no_script(&r.body);
    }

    #[test]
    fn search_finds_pages() {
        let s = store();
        let r = route(&s, "/search", "q=assessment");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("Assessment Plan"));
        no_script(&r.body);
    }

    #[test]
    fn search_empty_query_prompts() {
        let s = store();
        let r = route(&s, "/search", "q=");
        assert_eq!(r.status, 200);
        assert!(r.body.to_lowercase().contains("query"));
    }

    #[test]
    fn diagnostics_lists_broken_link() {
        let s = store();
        let r = route(&s, "/diagnostics", "");
        assert_eq!(r.status, 200);
        assert!(r.body.contains("link.broken"));
        no_script(&r.body);
    }

    #[test]
    fn dynamic_text_is_escaped() {
        assert_eq!(escape_html("<b>&\"'"), "&lt;b&gt;&amp;&quot;&#39;");
    }

    #[test]
    fn query_value_decodes() {
        assert_eq!(query_value("q=hello+world&x=1", "q"), "hello world");
        assert_eq!(query_value("q=a%2Fb", "q"), "a/b");
    }

    #[test]
    fn newlines_normalise_to_lf() {
        assert_eq!(normalize_newlines("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(normalize_newlines("plain\n"), "plain\n");
    }

    #[test]
    fn source_hash_is_stable_and_content_sensitive() {
        assert_eq!(source_hash("x"), source_hash("x"));
        assert_ne!(source_hash("x"), source_hash("y"));
        assert_eq!(source_hash("").len(), 16);
    }

    #[test]
    fn every_route_is_script_free() {
        let s = store();
        for (path, query) in [
            ("/", ""),
            ("/diagnostics", ""),
            ("/search", "q=e"),
            (&format!("/page/{HOME_ID}"), ""),
            ("/page/missing", ""),
        ] {
            no_script(&route(&s, path, query).body);
        }
    }
}
