// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Oracles shared by the fuzz targets.
//!
//! A fuzz target that only checks "did not panic" finds crashes. The targets
//! here also check the project's invariants on every input. The main oracle is
//! [`html_violation`]: rendered HTML may carry no way to run script, whatever
//! Markdown went in.

/// Elements that can run script, load active content, or redirect a page.
const FORBIDDEN_TAGS: &[&str] = &[
    "script", "iframe", "object", "embed", "applet", "frame", "frameset", "base", "meta", "link",
    "style", "form", "svg", "math",
];

/// Attributes whose value is a URL a browser will follow or load.
const URL_ATTRS: &[&str] = &[
    "href",
    "src",
    "action",
    "formaction",
    "xlink:href",
    "srcset",
];

/// Decode the HTML character references a browser would decode inside an
/// attribute value, so an entity-encoded `javascript:` is still caught.
fn decode_entities(v: &str) -> String {
    let mut out = String::new();
    let mut rest = v;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|&e| e <= 12);
        let decoded = end.and_then(|e| {
            let ent = &rest[1..e];
            let c = if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = ent.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                match ent {
                    "amp" => Some('&'),
                    "colon" => Some(':'),
                    "tab" => Some('\t'),
                    "newline" => Some('\n'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => None,
                }
            };
            c.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether a URL attribute value would run script or load active content.
fn dangerous_url(tag: &str, attr: &str, value: &str) -> bool {
    // Browsers ignore ASCII whitespace and control characters inside a scheme.
    let v: String = decode_entities(value)
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();
    if v.starts_with("javascript:") || v.starts_with("vbscript:") {
        return true;
    }
    if v.starts_with("data:") {
        // An inline image is inert as an <img>; any other data: URL is not.
        return !(tag == "img" && attr == "src" && v.starts_with("data:image/"));
    }
    false
}

/// The first way `html` could run script, or `None` if it cannot.
///
/// The renderer escapes text, so every literal `<` that survives in the output
/// opens a real tag; scanning tags is therefore enough.
pub fn html_violation(html: &str) -> Option<String> {
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let Some(len) = html[start..].find('>') else {
            return None; // an unterminated `<` cannot open a tag
        };
        let inner = &html[start..start + len];
        i = start + len + 1;
        if inner.starts_with('!') || inner.starts_with('/') || inner.starts_with('?') {
            continue;
        }
        let name: String = inner
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '/')
            .collect::<String>()
            .to_ascii_lowercase();
        if FORBIDDEN_TAGS.contains(&name.as_str()) {
            return Some(format!("forbidden element <{name}>"));
        }
        for (attr, value) in attributes(&inner[name.len()..]) {
            if attr.starts_with("on") {
                return Some(format!("event handler {attr} on <{name}>"));
            }
            if attr == "style" {
                return Some(format!("style attribute on <{name}>"));
            }
            if URL_ATTRS.contains(&attr.as_str()) && dangerous_url(&name, &attr, &value) {
                return Some(format!("dangerous {attr}={value:?} on <{name}>"));
            }
        }
    }
    None
}

/// The `(name, value)` pairs of a tag's attribute text, names lower-cased.
fn attributes(mut s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if s.is_empty() {
            return out;
        }
        let name_len = s
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(s.len());
        let name = s[..name_len].to_ascii_lowercase();
        s = s[name_len..].trim_start();
        let mut value = String::new();
        if let Some(rest) = s.strip_prefix('=') {
            let rest = rest.trim_start();
            if let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let body = &rest[1..];
                let end = body.find(q).unwrap_or(body.len());
                value = body[..end].to_string();
                s = body.get(end + 1..).unwrap_or("");
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                value = rest[..end].to_string();
                s = &rest[end..];
            }
        }
        if name.is_empty() {
            return out;
        }
        out.push((name, value));
    }
}

#[cfg(test)]
mod tests {
    use super::html_violation;

    #[test]
    fn the_oracle_passes_inert_html() {
        for ok in [
            "<p>hello &lt;script&gt;</p>",
            r#"<a href="https://example.org/x">x</a>"#,
            r#"<img src="data:image/png;base64,AAAA" alt="">"#,
            "<pre><code>javascript:alert(1)</code></pre>",
            "<!-- comment -->",
        ] {
            assert_eq!(html_violation(ok), None, "{ok}");
        }
    }

    #[test]
    fn the_oracle_catches_every_planted_script_route() {
        for bad in [
            "<script>x</script>",
            "<SCRIPT src=x>",
            r#"<img src=x onerror="alert(1)">"#,
            r#"<a href="javascript:alert(1)">x</a>"#,
            r#"<a href=" JaVa&#x53;cript&colon;alert(1)">x</a>"#,
            r#"<a href="java&#9;script:alert(1)">x</a>"#,
            r#"<a href="data:text/html,<b>">x</a>"#,
            r#"<iframe src="https://x">"#,
            r#"<p style="background:url(javascript:x)">"#,
            r#"<svg onload=alert(1)>"#,
        ] {
            assert!(html_violation(bad).is_some(), "missed: {bad}");
        }
    }
}
