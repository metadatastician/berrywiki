// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Who may talk to this server, decided before any route runs.
//!
//! The editor listens on a local port and changes files and git history, so
//! any web page the operator happens to visit is a potential client. Two
//! attacks matter:
//!
//! * *Cross-site request forgery:* a foreign page submits a form to
//!   `http://127.0.0.1:<port>/…`. A browser always labels such a `POST` with an
//!   `Origin` (and, in current browsers, `Sec-Fetch-Site: cross-site`), so any
//!   state-changing request whose origin is not this server's own is refused.
//! * *DNS rebinding:* a foreign name is re-pointed at 127.0.0.1, which makes
//!   the browser treat this server as same-origin with the attacker. The
//!   `Host` header still carries the foreign name, so only the names this
//!   server was bound to are answered.
//!
//! A client that sends neither `Origin` nor `Sec-Fetch-Site` (curl, a script
//! on the operator's own machine) is not a browser being steered by a page,
//! and is let through.
//!
//! The request head is also bounded here: line length, header count, and,
//! via [`ServerConfig::read_timeout`], how long a client may take, so that one
//! slow or hostile connection cannot stall a single-threaded server.

use std::io::{BufRead, Read};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::Response;

/// Longest accepted request line or header line, in bytes.
pub(crate) const MAX_HEAD_LINE: u64 = 8 * 1024;
/// Most header lines accepted in one request.
pub(crate) const MAX_HEADERS: usize = 100;
/// Default per-read socket timeout.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// The parts of a request head the server acts on.
#[derive(Debug, Default)]
pub(crate) struct Head {
    pub method: String,
    pub target: String,
    pub host: Option<String>,
    pub origin: Option<String>,
    pub sec_fetch_site: Option<String>,
    pub content_length: usize,
    pub content_type: String,
}

/// Why a request head was not accepted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HeadError {
    /// A line exceeded [`MAX_HEAD_LINE`] or there were more than [`MAX_HEADERS`].
    TooLarge,
    /// The connection ended or failed before the head was complete.
    Incomplete,
}

/// Read one line of at most [`MAX_HEAD_LINE`] bytes, without its line ending.
fn read_bounded_line<R: BufRead>(r: &mut R) -> Result<Option<String>, HeadError> {
    let mut buf = Vec::new();
    let n = r
        .by_ref()
        .take(MAX_HEAD_LINE + 1)
        .read_until(b'\n', &mut buf)
        .map_err(|_| HeadError::Incomplete)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        // Either the line is too long, or the peer stopped mid-line.
        return Err(if n as u64 > MAX_HEAD_LINE {
            HeadError::TooLarge
        } else {
            HeadError::Incomplete
        });
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Read and parse a request line and its headers, refusing oversized heads.
pub(crate) fn read_head<R: BufRead>(r: &mut R) -> Result<Head, HeadError> {
    let request_line = read_bounded_line(r)?.ok_or(HeadError::Incomplete)?;
    let mut parts = request_line.split_whitespace();
    let mut head = Head {
        method: parts.next().unwrap_or("").to_string(),
        target: parts.next().unwrap_or("/").to_string(),
        ..Head::default()
    };
    let mut count = 0usize;
    while let Some(line) = read_bounded_line(r)? {
        if line.is_empty() {
            break;
        }
        count += 1;
        if count > MAX_HEADERS {
            return Err(HeadError::TooLarge);
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        if k.eq_ignore_ascii_case("host") {
            head.host = Some(v.to_string());
        } else if k.eq_ignore_ascii_case("origin") {
            head.origin = Some(v.to_string());
        } else if k.eq_ignore_ascii_case("sec-fetch-site") {
            head.sec_fetch_site = Some(v.to_ascii_lowercase());
        } else if k.eq_ignore_ascii_case("content-length") {
            head.content_length = v.parse().unwrap_or(0);
        } else if k.eq_ignore_ascii_case("content-type") {
            head.content_type = v.to_string();
        }
    }
    Ok(head)
}

/// The `Host` values a server answers to.
#[derive(Debug, Clone)]
pub struct AllowedHosts {
    /// Lower-case `host[:port]` forms. Empty with `any` set means "any name".
    names: Vec<String>,
    any: bool,
}

impl AllowedHosts {
    /// The names a server bound at `addr` answers to.
    ///
    /// A loopback bind answers to its literal address and to `localhost`, with
    /// the port. A wildcard bind (`0.0.0.0`, `::`) answers to any name, because
    /// the server cannot know which of the machine's names a client used; the
    /// same-origin rule for state-changing requests still applies there. Any
    /// other bind answers to its literal address only. An address that does not
    /// parse answers to that exact string, nothing else.
    pub fn for_bind_addr(addr: &str) -> Self {
        let Ok(sa) = addr.parse::<SocketAddr>() else {
            return Self {
                names: vec![addr.to_ascii_lowercase()],
                any: false,
            };
        };
        let port = sa.port();
        let ip = sa.ip();
        if ip.is_unspecified() {
            return Self {
                names: Vec::new(),
                any: true,
            };
        }
        let literal = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        let mut hosts = vec![literal];
        if ip.is_loopback() {
            hosts.push("localhost".to_string());
        }
        let mut names = Vec::new();
        for h in hosts {
            names.push(format!("{h}:{port}"));
            if port == 80 {
                // Browsers omit the default port from Host.
                names.push(h);
            }
        }
        Self { names, any: false }
    }

    /// Whether a `Host` header value is one of this server's names.
    fn permits(&self, host: &str) -> bool {
        self.any || self.names.iter().any(|n| n.eq_ignore_ascii_case(host))
    }
}

/// How a server treats its connections.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Names the server answers to (DNS-rebinding defence).
    pub allowed_hosts: AllowedHosts,
    /// How long a single read on a connection may block.
    pub read_timeout: Duration,
}

impl ServerConfig {
    /// The defaults for a server bound at `addr`.
    pub fn for_bind_addr(addr: &str) -> Self {
        Self {
            allowed_hosts: AllowedHosts::for_bind_addr(addr),
            read_timeout: DEFAULT_READ_TIMEOUT,
        }
    }
}

/// A plain-text refusal with status `status`.
fn refusal(status: u16, why: &str) -> Response {
    Response::html(status, format!("<h1>{status} Refused</h1><p>{why}</p>"))
}

/// The response that refuses this request before routing, or `None` to
/// proceed. Applies to every method: the `Host` check guards reads too, since
/// a rebinding attack reads private pages before it writes.
pub(crate) fn refuse(head: &Head, allowed: &AllowedHosts) -> Option<Response> {
    let Some(host) = head.host.as_deref() else {
        return Some(refusal(400, "A Host header is required."));
    };
    if !allowed.permits(host) {
        return Some(refusal(
            403,
            "This server does not answer to that host name.",
        ));
    }
    if head.method != "GET" && head.method != "HEAD" {
        if let Some(origin) = head.origin.as_deref() {
            let own = format!("http://{host}");
            if !origin.eq_ignore_ascii_case(&own) {
                return Some(refusal(
                    403,
                    "Cross-origin requests may not change this wiki.",
                ));
            }
        }
        if let Some(site) = head.sec_fetch_site.as_deref() {
            if site != "same-origin" && site != "none" {
                return Some(refusal(
                    403,
                    "Cross-site requests may not change this wiki.",
                ));
            }
        }
    }
    None
}

/// The response for a head that could not be accepted, if one should be sent.
pub(crate) fn head_error_response(e: &HeadError) -> Option<Response> {
    match e {
        HeadError::TooLarge => Some(refusal(431, "The request head is too large.")),
        HeadError::Incomplete => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(method: &str, host: Option<&str>, origin: Option<&str>, site: Option<&str>) -> Head {
        Head {
            method: method.into(),
            target: "/".into(),
            host: host.map(Into::into),
            origin: origin.map(Into::into),
            sec_fetch_site: site.map(Into::into),
            ..Head::default()
        }
    }

    fn local() -> AllowedHosts {
        AllowedHosts::for_bind_addr("127.0.0.1:23779")
    }

    #[test]
    fn loopback_bind_answers_to_its_address_and_localhost_only() {
        let a = local();
        assert!(a.permits("127.0.0.1:23779"));
        assert!(a.permits("LOCALHOST:23779"));
        assert!(!a.permits("127.0.0.1:9999"));
        assert!(!a.permits("evil.example:23779"));
        assert!(!a.permits("127.0.0.1"));
    }

    #[test]
    fn ipv6_loopback_is_bracketed() {
        let a = AllowedHosts::for_bind_addr("[::1]:8080");
        assert!(a.permits("[::1]:8080"));
        assert!(a.permits("localhost:8080"));
        assert!(!a.permits("::1:8080"));
    }

    #[test]
    fn missing_host_is_refused() {
        let r = refuse(&head("GET", None, None, None), &local()).unwrap();
        assert_eq!(r.status, 400);
    }

    #[test]
    fn a_rebound_host_name_is_refused_even_for_a_read() {
        let r = refuse(
            &head("GET", Some("evil.example:23779"), None, None),
            &local(),
        )
        .unwrap();
        assert_eq!(r.status, 403);
    }

    #[test]
    fn a_cross_origin_post_is_refused() {
        let h = head(
            "POST",
            Some("127.0.0.1:23779"),
            Some("https://evil.example"),
            None,
        );
        assert_eq!(refuse(&h, &local()).unwrap().status, 403);
    }

    #[test]
    fn a_null_origin_post_is_refused() {
        let h = head("POST", Some("127.0.0.1:23779"), Some("null"), None);
        assert_eq!(refuse(&h, &local()).unwrap().status, 403);
    }

    #[test]
    fn a_cross_site_fetch_without_origin_is_refused() {
        let h = head("POST", Some("127.0.0.1:23779"), None, Some("cross-site"));
        assert_eq!(refuse(&h, &local()).unwrap().status, 403);
        let h = head("POST", Some("127.0.0.1:23779"), None, Some("same-site"));
        assert_eq!(refuse(&h, &local()).unwrap().status, 403);
    }

    #[test]
    fn same_origin_and_non_browser_posts_proceed() {
        let ok = [
            head(
                "POST",
                Some("127.0.0.1:23779"),
                Some("http://127.0.0.1:23779"),
                Some("same-origin"),
            ),
            head(
                "POST",
                Some("localhost:23779"),
                Some("http://localhost:23779"),
                None,
            ),
            head("POST", Some("127.0.0.1:23779"), None, Some("none")),
            head("POST", Some("127.0.0.1:23779"), None, None),
        ];
        for h in &ok {
            assert!(refuse(h, &local()).is_none(), "{h:?}");
        }
    }

    #[test]
    fn origin_must_match_the_host_the_request_used() {
        // Same server, but the page was loaded under the other name.
        let h = head(
            "POST",
            Some("127.0.0.1:23779"),
            Some("http://localhost:23779"),
            None,
        );
        assert_eq!(refuse(&h, &local()).unwrap().status, 403);
    }

    #[test]
    fn wildcard_bind_answers_any_name_but_still_needs_same_origin() {
        let any = AllowedHosts::for_bind_addr("0.0.0.0:23779");
        assert!(refuse(&head("GET", Some("box.lan:23779"), None, None), &any).is_none());
        let h = head(
            "POST",
            Some("box.lan:23779"),
            Some("http://evil.example"),
            None,
        );
        assert_eq!(refuse(&h, &any).unwrap().status, 403);
    }

    #[test]
    fn head_parsing_reads_the_guarded_headers() {
        let raw = b"POST /new HTTP/1.1\r\nHost: 127.0.0.1:1\r\nOrigin: http://x\r\n\
Sec-Fetch-Site: Cross-Site\r\nContent-Length: 5\r\nContent-Type: a/b\r\n\r\nhello";
        let h = read_head(&mut &raw[..]).unwrap();
        assert_eq!(h.method, "POST");
        assert_eq!(h.target, "/new");
        assert_eq!(h.host.as_deref(), Some("127.0.0.1:1"));
        assert_eq!(h.origin.as_deref(), Some("http://x"));
        assert_eq!(h.sec_fetch_site.as_deref(), Some("cross-site"));
        assert_eq!(h.content_length, 5);
        assert_eq!(h.content_type, "a/b");
    }

    #[test]
    fn an_overlong_header_line_is_refused() {
        let mut raw = b"GET / HTTP/1.1\r\nX-Pad: ".to_vec();
        raw.extend(std::iter::repeat_n(b'a', MAX_HEAD_LINE as usize + 10));
        raw.extend(b"\r\n\r\n");
        assert_eq!(read_head(&mut &raw[..]).unwrap_err(), HeadError::TooLarge);
    }

    #[test]
    fn too_many_headers_are_refused() {
        let mut raw = b"GET / HTTP/1.1\r\n".to_vec();
        for i in 0..=MAX_HEADERS {
            raw.extend(format!("X-{i}: v\r\n").bytes());
        }
        raw.extend(b"\r\n");
        assert_eq!(read_head(&mut &raw[..]).unwrap_err(), HeadError::TooLarge);
    }

    #[test]
    fn a_head_cut_off_mid_line_is_incomplete_not_accepted() {
        let raw = b"POST /new HTTP/1.1\r\nHost: 127.0.0.1:1\r\nOri";
        assert_eq!(read_head(&mut &raw[..]).unwrap_err(), HeadError::Incomplete);
    }
}
