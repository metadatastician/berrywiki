// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! A pull reader for the XML subset CherryTree writes.
//!
//! This is not a general XML parser and does not try to be one, for the same
//! reason ADR-0011 hand-rolls multipart: a whole XML crate to read one
//! well-known document shape would be a poor trade against a workspace whose
//! only third-party dependency is `comrak`.
//!
//! What it implements: elements, attributes in either quote style, the five
//! named entities plus numeric character references, self-closing tags,
//! CDATA sections (as ordinary text, so nothing inside one is lost), and the
//! XML declaration, comments and doctype (skipped).
//!
//! What it deliberately does **not** implement, each degrading to an
//! [`XmlError`] naming the construct rather than to a panic or a wrong
//! reading: namespace resolution, DTD-defined entities, external entities,
//! processing instructions with meaning, and encodings other than UTF-8.
//! CherryTree writes none of them.
//!
//! Refusing an unknown entity matters more than it looks. Silently dropping
//! `&nbsp;` would delete a character from a note; emitting it verbatim and
//! saying so leaves the text intact and the reader informed.

/// A parse failure, reported at the byte offset where it was noticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    /// Stable diagnostic code, e.g. `import.xml-unsupported`.
    pub code: &'static str,
    pub message: String,
    pub offset: usize,
}

impl XmlError {
    fn at(code: &'static str, message: impl Into<String>, offset: usize) -> Self {
        XmlError {
            code,
            message: message.into(),
            offset,
        }
    }
}

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}: {}", self.code, self.offset, self.message)
    }
}

/// One thing the reader found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    End {
        name: String,
    },
    /// Character data, already entity-decoded.
    Text(String),
}

impl Event {
    /// The value of `key`, if this is a start tag that carries it.
    pub fn attr(&self, key: &str) -> Option<&str> {
        match self {
            Event::Start { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }
}

pub struct Reader<'a> {
    src: &'a str,
    pos: usize,
    /// Entities the document used that this reader does not define. The text
    /// keeps them verbatim; the caller turns these into diagnostics.
    pub unknown_entities: Vec<String>,
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a str) -> Self {
        Reader {
            src,
            pos: 0,
            unknown_entities: Vec::new(),
        }
    }

    /// The next event, or `None` at end of input.
    pub fn next_event(&mut self) -> Result<Option<Event>, XmlError> {
        loop {
            if self.pos >= self.src.len() {
                return Ok(None);
            }
            let bytes = self.src.as_bytes();
            if bytes[self.pos] != b'<' {
                // Character data up to the next '<' (or end of input).
                let end = self.src[self.pos..]
                    .find('<')
                    .map(|i| self.pos + i)
                    .unwrap_or(self.src.len());
                let raw = &self.src[self.pos..end];
                self.pos = end;
                return Ok(Some(Event::Text(self.decode(raw))));
            }

            // A markup construct. Distinguish by the bytes after '<'.
            let rest = &self.src[self.pos..];
            if rest.starts_with("<!--") {
                self.pos = self.skip_to("-->", 4, "unterminated comment")?;
                continue;
            }
            if rest.starts_with("<![CDATA[") {
                let end = self.src[self.pos + 9..].find("]]>").ok_or_else(|| {
                    XmlError::at(
                        "import.xml-malformed",
                        "unterminated CDATA section",
                        self.pos,
                    )
                })?;
                let text = self.src[self.pos + 9..self.pos + 9 + end].to_string();
                self.pos = self.pos + 9 + end + 3;
                // CDATA is literal by definition: no entity decoding.
                return Ok(Some(Event::Text(text)));
            }
            if rest.starts_with("<!DOCTYPE") {
                self.pos = self.skip_to(">", 9, "unterminated doctype")?;
                continue;
            }
            if rest.starts_with("<?") {
                self.pos = self.skip_to("?>", 2, "unterminated processing instruction")?;
                continue;
            }
            if rest.starts_with("<!") {
                return Err(XmlError::at(
                    "import.xml-unsupported",
                    "declaration other than a comment, CDATA section or doctype",
                    self.pos,
                ));
            }
            if rest.starts_with("</") {
                let end = rest.find('>').ok_or_else(|| {
                    XmlError::at("import.xml-malformed", "unterminated end tag", self.pos)
                })?;
                let name = rest[2..end].trim().to_string();
                if name.is_empty() {
                    return Err(XmlError::at(
                        "import.xml-malformed",
                        "end tag with no name",
                        self.pos,
                    ));
                }
                self.pos += end + 1;
                return Ok(Some(Event::End { name }));
            }
            return self.start_tag().map(Some);
        }
    }

    fn skip_to(&self, needle: &str, from: usize, what: &'static str) -> Result<usize, XmlError> {
        let start = self.pos + from;
        let idx = self.src[start..]
            .find(needle)
            .ok_or_else(|| XmlError::at("import.xml-malformed", what, self.pos))?;
        Ok(start + idx + needle.len())
    }

    fn start_tag(&mut self) -> Result<Event, XmlError> {
        let open = self.pos;
        let mut i = open + 1;
        let bytes = self.src.as_bytes();

        let name_start = i;
        while i < bytes.len() && !is_space(bytes[i]) && bytes[i] != b'>' && bytes[i] != b'/' {
            i += 1;
        }
        if i == name_start {
            return Err(XmlError::at(
                "import.xml-malformed",
                "start tag with no name",
                open,
            ));
        }
        let name = self.src[name_start..i].to_string();

        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            while i < bytes.len() && is_space(bytes[i]) {
                i += 1;
            }
            if i >= bytes.len() {
                return Err(XmlError::at(
                    "import.xml-malformed",
                    format!("unterminated start tag <{name}>"),
                    open,
                ));
            }
            if bytes[i] == b'>' {
                self.pos = i + 1;
                return Ok(Event::Start {
                    name,
                    attrs,
                    self_closing: false,
                });
            }
            if bytes[i] == b'/' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    self.pos = i + 2;
                    return Ok(Event::Start {
                        name,
                        attrs,
                        self_closing: true,
                    });
                }
                return Err(XmlError::at(
                    "import.xml-malformed",
                    format!("stray '/' in <{name}>"),
                    i,
                ));
            }

            let key_start = i;
            while i < bytes.len() && !is_space(bytes[i]) && bytes[i] != b'=' && bytes[i] != b'>' {
                i += 1;
            }
            let key = self.src[key_start..i].to_string();
            if key.is_empty() {
                return Err(XmlError::at(
                    "import.xml-malformed",
                    format!("attribute with no name in <{name}>"),
                    i,
                ));
            }
            while i < bytes.len() && is_space(bytes[i]) {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'=' {
                // A valueless attribute is HTML, not XML. Refuse rather than
                // guess what it was supposed to mean.
                return Err(XmlError::at(
                    "import.xml-unsupported",
                    format!("attribute '{key}' in <{name}> has no value"),
                    i,
                ));
            }
            i += 1;
            while i < bytes.len() && is_space(bytes[i]) {
                i += 1;
            }
            if i >= bytes.len() || (bytes[i] != b'"' && bytes[i] != b'\'') {
                return Err(XmlError::at(
                    "import.xml-malformed",
                    format!("attribute '{key}' in <{name}> is not quoted"),
                    i,
                ));
            }
            let quote = bytes[i] as char;
            let val_start = i + 1;
            let val_end = self.src[val_start..].find(quote).ok_or_else(|| {
                XmlError::at(
                    "import.xml-malformed",
                    format!("unterminated value for attribute '{key}'"),
                    val_start,
                )
            })? + val_start;
            let raw = &self.src[val_start..val_end];
            let value = self.decode(raw);
            attrs.push((key, value));
            i = val_end + 1;
        }
    }

    /// Replace entity references. Unknown ones are kept verbatim and recorded,
    /// so text is never silently shortened.
    fn decode(&mut self, raw: &str) -> String {
        if !raw.contains('&') {
            return raw.to_string();
        }
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(amp) = rest.find('&') {
            out.push_str(&rest[..amp]);
            let tail = &rest[amp..];
            let Some(semi) = tail.find(';') else {
                // A bare '&' is not an entity at all; keep it.
                out.push('&');
                rest = &tail[1..];
                continue;
            };
            let entity = &tail[1..semi];
            if !is_entity_name(entity) {
                // "AT&T; the company" has an ampersand and a semicolon but no
                // entity between them. Consuming to the ';' would swallow real
                // punctuation and report an entity nobody wrote, so the '&' is
                // literal and scanning resumes right after it.
                out.push('&');
                rest = &tail[1..];
                continue;
            }
            match decode_entity(entity) {
                Some(c) => out.push(c),
                None => {
                    out.push_str(&tail[..=semi]);
                    let named = format!("&{entity};");
                    if !self.unknown_entities.contains(&named) {
                        self.unknown_entities.push(named);
                    }
                }
            }
            rest = &tail[semi + 1..];
        }
        out.push_str(rest);
        out
    }
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = entity.strip_prefix('#')?;
            let n = match digits
                .strip_prefix('x')
                .or_else(|| digits.strip_prefix('X'))
            {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse::<u32>().ok()?,
            };
            char::from_u32(n)
        }
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Is `entity` shaped like an entity name at all?
///
/// This is what separates `&amp;` and `&#233;`, which are entities, from the
/// `&` in `AT&T; the company`, which is a literal ampersand that happens to
/// have a semicolon later in the line.
fn is_entity_name(entity: &str) -> bool {
    match entity.strip_prefix('#') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_alphanumeric()),
        None => !entity.is_empty() && entity.bytes().all(|b| b.is_ascii_alphanumeric()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the reader to the end, collecting events.
    fn all(src: &str) -> Result<Vec<Event>, XmlError> {
        let mut r = Reader::new(src);
        let mut out = Vec::new();
        while let Some(ev) = r.next_event()? {
            out.push(ev);
        }
        Ok(out)
    }

    /// Drive the reader and also return what it could not decode.
    fn all_with_unknowns(src: &str) -> (Vec<Event>, Vec<String>) {
        let mut r = Reader::new(src);
        let mut out = Vec::new();
        while let Some(ev) = r.next_event().expect("well-formed fixture") {
            out.push(ev);
        }
        (out, r.unknown_entities)
    }

    fn text_of(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn elements_attributes_and_text() {
        let events = all(r#"<node id="7" name="Notes">hello</node>"#).expect("well formed");
        assert_eq!(events.len(), 3, "start, text, end");
        match &events[0] {
            Event::Start {
                name,
                attrs,
                self_closing,
            } => {
                assert_eq!(name, "node");
                assert_eq!(attrs.len(), 2);
                assert!(!self_closing);
            }
            other => panic!("expected a start tag, got {other:?}"),
        }
        assert_eq!(events[0].attr("id"), Some("7"));
        assert_eq!(events[0].attr("name"), Some("Notes"));
        assert_eq!(events[0].attr("absent"), None);
        assert_eq!(events[1], Event::Text("hello".to_string()));
        assert_eq!(
            events[2],
            Event::End {
                name: "node".to_string()
            }
        );
    }

    #[test]
    fn attr_is_only_read_from_a_start_tag() {
        // A caller that asks an End or Text event for an attribute gets None
        // rather than a wrong answer from a neighbouring tag.
        let end = Event::End {
            name: "node".to_string(),
        };
        assert_eq!(end.attr("id"), None);
        assert_eq!(Event::Text("id".to_string()).attr("id"), None);
    }

    #[test]
    fn both_quote_styles_and_awkward_spacing() {
        let events = all("<a  x = 'one'   y=\"two\"  >t</a>").expect("well formed");
        assert_eq!(events[0].attr("x"), Some("one"));
        assert_eq!(events[0].attr("y"), Some("two"));
        // A quote of the other kind inside a value is ordinary data.
        let events = all(r#"<a x='say "hi"' y="it's"></a>"#).expect("well formed");
        assert_eq!(events[0].attr("x"), Some(r#"say "hi""#));
        assert_eq!(events[0].attr("y"), Some("it's"));
    }

    #[test]
    fn self_closing_tags_report_themselves() {
        let events = all("<a><br/><hr /></a>").expect("well formed");
        let closing: Vec<bool> = events
            .iter()
            .filter_map(|e| match e {
                Event::Start { self_closing, .. } => Some(*self_closing),
                _ => None,
            })
            .collect();
        assert_eq!(closing, vec![false, true, true]);
        // A self-closing tag emits no End event, so a caller tracking depth
        // must consult the flag rather than counting End events.
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::End { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn named_and_numeric_entities_decode() {
        let (events, unknown) =
            all_with_unknowns("<a>&amp;&lt;&gt;&quot;&apos;&#65;&#x41;&#X41;&#233;</a>");
        assert_eq!(text_of(&events), "&<>\"'AAA\u{e9}");
        assert!(unknown.is_empty(), "all of those are defined: {unknown:?}");
        // And in attribute values, which take the same path.
        let events = all(r#"<a t="1 &lt; 2 &amp;&amp; 3 &gt; 2"></a>"#).expect("well formed");
        assert_eq!(events[0].attr("t"), Some("1 < 2 && 3 > 2"));
    }

    #[test]
    fn unknown_entity_is_kept_verbatim_and_reported() {
        // The whole point: dropping &nbsp; would silently delete a character
        // from someone's note. Keeping it costs nothing and is honest.
        let (events, unknown) = all_with_unknowns("<a>5&nbsp;6 &nbsp; 7 &copy;</a>");
        assert_eq!(text_of(&events), "5&nbsp;6 &nbsp; 7 &copy;");
        assert_eq!(unknown, vec!["&nbsp;".to_string(), "&copy;".to_string()]);
    }

    #[test]
    fn out_of_range_numeric_reference_is_unknown_not_a_panic() {
        // A lone surrogate is not a char. char::from_u32 says so, and the
        // reference survives as text instead of taking the process down.
        let (events, unknown) = all_with_unknowns("<a>&#xD800;&#x110000;&#999999999999;</a>");
        assert_eq!(text_of(&events), "&#xD800;&#x110000;&#999999999999;");
        assert_eq!(unknown.len(), 3);
    }

    #[test]
    fn an_ampersand_that_starts_nothing_entity_shaped_is_literal() {
        // The line is drawn at shape, not at whether the name is defined.
        // Nothing between the '&' and the ';' here could be an entity name,
        // so the '&' is data and scanning resumes right after it. Without the
        // is_entity_name guard the reader consumes to the ';', swallowing real
        // punctuation and reporting an entity nobody wrote.
        for src in [
            "<a>a & b</a>",
            "<a>Tom & Jerry; and friends</a>",
            "<a>&</a>",
            "<a>& ;</a>",
            "<a>&;</a>",
            "<a>&#;</a>",
        ] {
            let (events, unknown) = all_with_unknowns(src);
            let inner = &src["<a>".len()..src.len() - "</a>".len()];
            assert_eq!(text_of(&events), inner, "text changed for {src}");
            assert!(
                unknown.is_empty(),
                "invented an entity in {src}: {unknown:?}"
            );
        }
    }

    #[test]
    fn an_entity_shaped_reference_is_reported_even_when_it_reads_like_prose() {
        // The other side of that line. "AT&T;" looks like prose to a human,
        // but "&T;" is a well-formed entity reference by XML's own grammar and
        // this reader defines no "T". Saying so is the correct reading: a real
        // CherryTree file writes "&amp;", so a bare one means the file either
        // is not well-formed or leans on a DTD entity this reader refuses.
        // Either way the text is kept and the caller is told.
        let (events, unknown) = all_with_unknowns("<a>AT&T; the company</a>");
        assert_eq!(text_of(&events), "AT&T; the company");
        assert_eq!(unknown, vec!["&T;".to_string()]);
    }

    #[test]
    fn cdata_is_literal_and_arrives_as_its_own_event() {
        let events = all("<a>before<![CDATA[x &amp; y < z]]>after</a>").expect("well formed");
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        // Three separate text events, so a caller must concatenate. And
        // nothing inside the section is decoded: that is what CDATA means.
        assert_eq!(texts, vec!["before", "x &amp; y < z", "after"]);
    }

    #[test]
    fn declaration_comment_and_doctype_are_skipped() {
        let src = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                   <!DOCTYPE cherrytree>\n\
                   <!-- a comment with <tags> and &amp; inside -->\n\
                   <a>t</a>";
        let events = all(src).expect("well formed");
        let starts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::Start { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec!["a"], "only the real element survives");
        assert_eq!(text_of(&events).trim(), "t");
    }

    #[test]
    fn multibyte_text_names_and_values_survive() {
        // The scanner indexes bytes. Every byte it stops on is ASCII, so a
        // slice can never land mid-character, but that is a property worth a
        // test rather than an argument.
        let src = concat!(
            "<n\u{e9}ud nom=\"caf\u{e9} \u{4e2d}\u{6587}\">",
            "t\u{e9}xt \u{4e2d}\u{6587} \u{1f600}",
            "</n\u{e9}ud>"
        );
        let events = all(src).expect("well formed");
        assert_eq!(events[0].attr("nom"), Some("caf\u{e9} \u{4e2d}\u{6587}"));
        assert_eq!(text_of(&events), "t\u{e9}xt \u{4e2d}\u{6587} \u{1f600}");
        match &events[2] {
            Event::End { name } => assert_eq!(name, "n\u{e9}ud"),
            other => panic!("expected an end tag, got {other:?}"),
        }
    }

    #[test]
    fn malformed_input_is_an_error_with_a_code_and_a_real_offset() {
        // Each case names the construct rather than guessing at a repair.
        let cases: [(&str, &str); 7] = [
            ("  <a", "import.xml-malformed"),         // unterminated start tag
            ("<a x='1", "import.xml-malformed"),      // unterminated value
            ("<a x=1>", "import.xml-malformed"),      // unquoted value
            ("<a b>", "import.xml-unsupported"),      // valueless attribute
            ("<a><!-- open", "import.xml-malformed"), // unterminated comment
            ("<a><![CDATA[open", "import.xml-malformed"),
            ("<a></>", "import.xml-malformed"), // end tag with no name
        ];
        for (src, code) in cases {
            let err = all(src).expect_err(&format!("{src} should not parse"));
            assert_eq!(err.code, code, "wrong code for {src}");
            assert!(
                err.offset <= src.len(),
                "offset {} past the end of {src}",
                err.offset
            );
            assert!(!err.message.is_empty(), "no message for {src}");
        }
    }

    #[test]
    fn error_offsets_point_at_the_construct() {
        // Two offsets computed by hand, so a future change that reports byte 0
        // for everything fails here rather than passing the bound above.
        let err = all("  <a").expect_err("unterminated");
        assert_eq!(err.offset, 2, "the '<' of the unterminated tag");
        let err = all("<a b>").expect_err("valueless attribute");
        assert_eq!(err.offset, 4, "the '>' where a value was due");
        assert!(err.to_string().contains("at byte 4"), "{err}");
    }

    #[test]
    fn an_unsupported_declaration_is_refused_rather_than_guessed_at() {
        let err = all("<a><!ENTITY x \"y\"></a>").expect_err("DTD entity");
        assert_eq!(err.code, "import.xml-unsupported");
    }

    #[test]
    fn reading_the_same_source_twice_gives_the_same_events() {
        let src = r#"<cherrytree><node id="1" name="A">x &amp; &nbsp; y</node></cherrytree>"#;
        let (first, first_unknown) = all_with_unknowns(src);
        let (second, second_unknown) = all_with_unknowns(src);
        assert_eq!(first, second);
        assert_eq!(first_unknown, second_unknown);
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        // A notebook is user data and may be truncated, corrupted, or not
        // CherryTree at all. Every one of those must be an error or an event
        // stream, never a panic. Seeded so a failure is reproducible.
        let alphabet: [&str; 20] = [
            "<",
            ">",
            "/",
            "!",
            "?",
            "-",
            "[",
            "]",
            "&",
            ";",
            "\"",
            "'",
            "=",
            " ",
            "\n",
            "a",
            "1",
            "\u{e9}",
            "\u{4e2d}",
            "\u{1f600}",
        ];
        let mut seed: u64 = 0x5eed_1234_abcd_ef01;
        for _ in 0..4000 {
            let mut src = String::new();
            // Length varies so truncation at every stage gets exercised.
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let len = (seed >> 33) as usize % 40;
            for _ in 0..len {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                src.push_str(alphabet[(seed >> 33) as usize % alphabet.len()]);
            }
            let mut r = Reader::new(&src);
            let mut guard = 0;
            loop {
                guard += 1;
                assert!(guard < 10_000, "reader failed to advance on {src:?}");
                match r.next_event() {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }
}
