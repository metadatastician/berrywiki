// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! End-to-end attacks against a real listening server (v1 criteria S2, S3).
//!
//! Each attack is sent as raw bytes over a TCP socket, exactly as a hostile
//! page's browser would send it, and must be refused *and leave the wiki
//! unchanged*. A positive control sends the same payload from the server's
//! own origin and shows it does take effect, so a refusal here means the guard
//! stopped a request that would otherwise have worked.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use berrywiki_draft::DraftStore;
use berrywiki_serve::{serve_listener, App, ServerConfig};
use berrywiki_store::LocalFolderStore;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh empty directory under the system temp dir.
fn scratch_dir(kind: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "berrywiki-provenance-{kind}-{}-{}",
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

/// Start an editor on an ephemeral loopback port and return its address and
/// wiki folder. The server thread lives until the test process exits.
fn start_server(read_timeout: Duration) -> (String, PathBuf) {
    start_server_with(read_timeout, berrywiki_serve::DEFAULT_CONNECTION_DEADLINE)
}

/// [`start_server`] with an explicit total per-connection deadline.
fn start_server_with(read_timeout: Duration, deadline: Duration) -> (String, PathBuf) {
    let wiki = scratch_wiki();
    let drafts = scratch_dir("drafts");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let mut config = ServerConfig::for_bind_addr(&addr);
    config.read_timeout = read_timeout;
    config.connection_deadline = deadline;
    let wiki_for_thread = wiki.clone();
    std::thread::spawn(move || {
        let mut app = App::with_drafts(
            LocalFolderStore::open(&wiki_for_thread).unwrap(),
            Some(DraftStore::new(&drafts)),
        );
        let _ = serve_listener(&mut app, listener, &config);
    });
    (addr, wiki)
}

/// Send raw request bytes and return the status code and the full response.
fn send(addr: &str, raw: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    let status = out
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, out)
}

/// A form `POST /new` creating a page titled `title`, with extra header lines.
fn new_page_request(host: &str, title: &str, extra_headers: &str) -> String {
    let body = format!("title={title}&parent=&body=planted&tags=&action=save");
    format!(
        "POST /new HTTP/1.1\r\nHost: {host}\r\n{extra_headers}\
Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Whether any page file in the wiki mentions `needle`.
fn wiki_contains(wiki: &Path, needle: &str) -> bool {
    fs::read_dir(wiki).unwrap().any(|e| {
        let p = e.unwrap().path();
        p.extension().is_some_and(|x| x == "md")
            && fs::read_to_string(&p).unwrap_or_default().contains(needle)
    })
}

#[test]
fn a_cross_site_form_post_is_refused_and_writes_nothing() {
    let (addr, wiki) = start_server(Duration::from_secs(5));
    let attacks = [
        ("CsrfOrigin", "Origin: https://evil.example\r\n"),
        ("CsrfNull", "Origin: null\r\n"),
        ("CsrfFetch", "Sec-Fetch-Site: cross-site\r\n"),
    ];
    for (title, header) in attacks {
        let (status, _) = send(&addr, &new_page_request(&addr, title, header));
        assert_eq!(status, 403, "{title} was not refused");
        assert!(!wiki_contains(&wiki, title), "{title} reached the wiki");
    }

    // Positive control: the same request from the server's own origin works,
    // so the refusals above stopped a request that would otherwise succeed.
    let own = format!("Origin: http://{addr}\r\nSec-Fetch-Site: same-origin\r\n");
    let (status, _) = send(&addr, &new_page_request(&addr, "OwnOrigin", &own));
    assert_eq!(status, 303, "a same-origin save must succeed");
    assert!(wiki_contains(&wiki, "OwnOrigin"));
}

#[test]
fn a_rebound_host_name_cannot_read_or_write() {
    let (addr, wiki) = start_server(Duration::from_secs(5));
    let port = addr.rsplit(':').next().unwrap();
    let rebound = format!("attacker.example:{port}");

    let (status, body) = send(&addr, &format!("GET / HTTP/1.1\r\nHost: {rebound}\r\n\r\n"));
    assert_eq!(status, 403);
    assert!(
        !body.contains("Home"),
        "page content leaked to a foreign Host"
    );

    let origin = format!("Origin: http://{rebound}\r\n");
    let (status, _) = send(&addr, &new_page_request(&rebound, "Rebound", &origin));
    assert_eq!(status, 403);
    assert!(!wiki_contains(&wiki, "Rebound"));

    // Positive control: the real name reads fine.
    let (status, _) = send(&addr, &format!("GET / HTTP/1.1\r\nHost: {addr}\r\n\r\n"));
    assert_eq!(status, 200);
}

#[test]
fn a_request_without_host_is_refused() {
    let (addr, _) = start_server(Duration::from_secs(5));
    let (status, _) = send(&addr, "GET / HTTP/1.1\r\n\r\n");
    assert_eq!(status, 400);
}

#[test]
fn responses_forbid_script_framing_and_sniffing() {
    let (addr, _) = start_server(Duration::from_secs(5));
    let (_, out) = send(&addr, &format!("GET / HTTP/1.1\r\nHost: {addr}\r\n\r\n"));
    let head = out.split("\r\n\r\n").next().unwrap();
    assert!(head.contains("script-src 'none'"), "{head}");
    assert!(head.contains("frame-ancestors 'none'"));
    assert!(head.contains("X-Content-Type-Options: nosniff"));
}

#[test]
fn a_stalled_client_does_not_block_the_next_one() {
    let (addr, _) = start_server(Duration::from_millis(300));
    // Opens a connection, sends half a request line, then goes silent.
    let mut stalled = TcpStream::connect(&addr).unwrap();
    stalled.write_all(b"GET / HT").unwrap();

    let started = Instant::now();
    let (status, _) = send(&addr, &format!("GET / HTTP/1.1\r\nHost: {addr}\r\n\r\n"));
    assert_eq!(status, 200);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "second client waited {:?} behind a stalled one",
        started.elapsed()
    );
    drop(stalled);
}

#[test]
fn an_oversized_head_is_refused_with_431() {
    let (addr, _) = start_server(Duration::from_secs(5));
    let pad = "a".repeat(9 * 1024);
    let (status, _) = send(
        &addr,
        &format!("GET / HTTP/1.1\r\nHost: {addr}\r\nX-Pad: {pad}\r\n\r\n"),
    );
    assert_eq!(status, 431);
}

#[test]
fn a_client_dripping_bytes_inside_each_timeout_is_cut_off() {
    // 300 ms per read, 1 s per connection in total.
    let (addr, _) = start_server_with(Duration::from_millis(300), Duration::from_secs(1));
    // One byte every 150 ms, always inside the per-read timeout, never done.
    let drip_addr = addr.clone();
    std::thread::spawn(move || {
        let mut s = TcpStream::connect(&drip_addr).unwrap();
        let line = b"GET / HTTP/1.1\r\nX-Drip: ";
        for b in line.iter().chain(std::iter::repeat(&b'a')) {
            if s.write_all(&[*b]).is_err() {
                break; // the server cut us off
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    });
    std::thread::sleep(Duration::from_millis(200));

    let started = Instant::now();
    let (status, _) = send(&addr, &format!("GET / HTTP/1.1\r\nHost: {addr}\r\n\r\n"));
    assert_eq!(status, 200);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "second client waited {:?} behind a dripping one",
        started.elapsed()
    );
}
