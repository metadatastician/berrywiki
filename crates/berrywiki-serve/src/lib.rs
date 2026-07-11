//! Zero-JavaScript, server-side-rendered read-only explorer for BerryWiki
//! (ADR-0005).
//!
//! The routing logic is a pure function — [`route`] maps a request path +
//! query to a [`Response`] against a `&LocalFolderStore` — so the whole UI is
//! testable in-process with no sockets. [`serve`] is a thin blocking accept
//! loop over `std::net`.
//!
//! Invariants:
//! * **No `<script>` ever ships.** Every response is HTML with inline CSS and
//!   plain forms/links; a test asserts no script element in any route.
//! * All dynamic text is HTML-escaped at the boundary; page *body* content is
//!   rendered by `berrywiki-render`, which escapes raw HTML and neutralises
//!   dangerous URL schemes.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use berrywiki_render::render_markdown;
use berrywiki_store::{LocalFolderStore, WikiStore};

/// A minimal HTTP response.
pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Response {
    fn html(status: u16, body: String) -> Self {
        Response {
            status,
            content_type: "text/html; charset=utf-8",
            body,
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    }
}

/// Route a GET request to a response. Pure and socket-free — the unit of test.
pub fn route(store: &LocalFolderStore, path: &str, query: &str) -> Response {
    if path == "/" {
        return home_page(store);
    }
    if path == "/diagnostics" {
        return diagnostics_page(store);
    }
    if path == "/search" {
        return search_page(store, &query_value(query, "q"));
    }
    if let Some(rest) = path.strip_prefix("/page/") {
        return page_view(store, &percent_decode(rest));
    }
    Response::html(
        404,
        layout(store, None, "Not found", "<p>No such page.</p>".to_string()),
    )
}

fn home_page(store: &LocalFolderStore) -> Response {
    // Land on the first root page if there is one, else an empty-state.
    if let Some(root) = store.graph().roots().first() {
        return page_view(store, &root.id);
    }
    Response::html(
        200,
        layout(
            store,
            None,
            "BerryWiki",
            "<p>This wiki has no pages yet.</p>".to_string(),
        ),
    )
}

fn page_view(store: &LocalFolderStore, id: &str) -> Response {
    let page = match store.read_page(id) {
        Ok(p) => p,
        Err(_) => {
            return Response::html(
                404,
                layout(
                    store,
                    None,
                    "Not found",
                    format!("<p>No page with id <code>{}</code>.</p>", escape_html(id)),
                ),
            )
        }
    };

    let rendered = render_markdown(&page.body);
    let main = format!("<article class=\"page\">{rendered}</article>");
    let aside = context_pane(store, id);
    let body = layout_three(store, Some(id), &page.title, &main, &aside);
    Response::html(200, body)
}

fn diagnostics_page(store: &LocalFolderStore) -> Response {
    let diags: Vec<String> = store
        .graph()
        .diagnostics()
        .iter()
        .chain(store.load_diagnostics().iter())
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
    Response::html(200, layout(store, None, "Diagnostics", main))
}

fn search_page(store: &LocalFolderStore, q: &str) -> Response {
    let needle = q.trim().to_lowercase();
    let main = if needle.is_empty() {
        "<p>Type a query above.</p>".to_string()
    } else {
        let mut hits = Vec::new();
        for page in store.graph().pages() {
            let in_title = page.title.to_lowercase().contains(&needle);
            let in_body = page.body.to_lowercase().contains(&needle);
            if in_title || in_body {
                hits.push(format!(
                    "<li><a href=\"/page/{}\">{}</a>{}</li>",
                    escape_attr(&page.id),
                    escape_html(&page.title),
                    if in_title { "" } else { " <small>(body)</small>" }
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
    Response::html(200, layout(store, None, "Search", main))
}

/// The right-hand context pane: outline, tags, backlinks.
fn context_pane(store: &LocalFolderStore, id: &str) -> String {
    let page = match store.read_page(id) {
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

    let backlinks = store.graph().backlinks_of(id);
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

/// The navigation tree (left pane).
fn nav_tree(store: &LocalFolderStore, current: Option<&str>) -> String {
    let mut out = String::from("<nav class=\"tree\" aria-label=\"Notebook\"><ul>");
    for (depth, page) in store.graph().walk() {
        let is_current = current == Some(page.id.as_str());
        let archived = if page.is_archived() { " archived" } else { "" };
        out.push_str(&format!(
            "<li style=\"--depth:{depth}\" class=\"tree-item{archived}{}\">\
             <a href=\"/page/{}\"{}>{}</a></li>",
            if is_current { " current" } else { "" },
            escape_attr(&page.id),
            if is_current { " aria-current=\"page\"" } else { "" },
            escape_html(&page.title),
        ));
    }
    out.push_str("</ul></nav>");
    out
}

/// Two-pane layout (nav + main), used for search/diagnostics/empty.
fn layout(store: &LocalFolderStore, current: Option<&str>, title: &str, main: String) -> String {
    layout_three(store, current, title, &main, "")
}

/// Three-pane layout (nav + main + aside).
fn layout_three(
    store: &LocalFolderStore,
    current: Option<&str>,
    title: &str,
    main: &str,
    aside: &str,
) -> String {
    let nav = nav_tree(store, current);
    let aside_html = if aside.is_empty() {
        String::new()
    } else {
        format!("<aside class=\"context\">{aside}</aside>")
    };
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title} — BerryWiki</title><style>{CSS}</style></head><body>\
<header class=\"topbar\"><a class=\"brand\" href=\"/\">BerryWiki</a>\
<form class=\"search\" method=\"get\" action=\"/search\" role=\"search\">\
<input type=\"search\" name=\"q\" placeholder=\"Search…\" aria-label=\"Search\">\
<button type=\"submit\">Search</button></form>\
<a class=\"diag-link\" href=\"/diagnostics\">Diagnostics</a></header>\
<div class=\"grid\">{nav}<main class=\"main\"><h1>{title_h1}</h1>{main}</main>{aside_html}</div>\
</body></html>",
        title = escape_html(title),
        title_h1 = escape_html(title),
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
@media(prefers-color-scheme:dark){body{background:#161616;color:#e6e6e6}.tree,.context{border-color:#333}.tree-item a{color:#cfcfcf}.tree-item a:hover{background:#222}.page pre{background:#222}}";

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
fn escape_attr(s: &str) -> String {
    escape_html(s)
}

/// Extract a query-string value by key, percent- and plus-decoding it.
fn query_value(query: &str, key: &str) -> String {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return percent_decode(&v.replace('+', " "));
            }
        }
    }
    String::new()
}

/// Percent-decode (`%XX`) a string. Invalid escapes are left literal.
fn percent_decode(s: &str) -> String {
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

// --- server ----------------------------------------------------------------

/// Blocking, single-threaded HTTP server for a single-user localhost session.
/// Serves GET requests via [`route`]; returns only on a listener error.
pub fn serve(store: &LocalFolderStore, addr: &str) -> io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                // A per-connection error must not bring the server down.
                let _ = handle_connection(&mut s, store);
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

fn handle_connection(stream: &mut TcpStream, store: &LocalFolderStore) -> io::Result<()> {
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

fn write_response(stream: &mut TcpStream, response: &Response) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason(response.status),
        response.content_type,
        response.body.as_bytes().len(),
    );
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
