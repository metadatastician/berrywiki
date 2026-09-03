// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Turning styled runs of text into Markdown that means the same thing.
//!
//! The hazard this module exists to avoid is silent reinterpretation. A note
//! containing `*` or a line beginning `# ` is ordinary prose in a rich-text
//! editor and is markup in Markdown. Copying it across unescaped does not
//! lose the text; it changes what the text says, which is worse, because the
//! import looks like it worked.

/// Inline styling carried by one run of source text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// 1, 2 or 3 when the run is a heading of that level.
    pub heading: Option<u8>,
    /// Target for a link wrapping this run: an absolute URL.
    pub link: Option<String>,
    /// Target for a wiki link wrapping this run, already resolved to a title.
    pub wikilink: Option<String>,
}

impl Marks {
    pub fn is_plain(&self) -> bool {
        *self == Marks::default()
    }
}

/// Escape the characters that would otherwise be read as inline markup.
///
/// Deliberately conservative: escaping a character that did not need it is
/// invisible once rendered, whereas missing one changes the text.
pub fn escape_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '&' | '|' | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape a leading character that would make the line a block construct.
///
/// Only the start of a line matters: `-` mid-sentence is a hyphen, but `- `
/// at column zero is a list item.
pub fn escape_line_start(line: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return line.to_string();
    }
    let indent_len = line.len() - trimmed.len();
    let (indent, rest) = line.split_at(indent_len);

    let needs_escape = |r: &str| -> bool {
        let first = r.as_bytes()[0];
        if matches!(first, b'#' | b'>' | b'-' | b'+' | b'=') {
            return true;
        }
        if first == b':' && r.starts_with("::") {
            return true;
        }
        // `1.` or `1)` at the start of a line is an ordered list.
        let digits = r.bytes().take_while(|b| b.is_ascii_digit()).count();
        digits > 0 && matches!(r.as_bytes().get(digits), Some(b'.') | Some(b')'))
    };

    if needs_escape(rest) {
        format!("{indent}\\{rest}")
    } else {
        line.to_string()
    }
}

/// Wrap `text` in a code span, choosing a backtick fence long enough that the
/// content cannot terminate it early.
pub fn code_span(text: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    // A space is needed when the content starts or ends with a backtick, so
    // the delimiters stay distinguishable from the content.
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// Render one run of same-styled text, which must not contain a newline.
pub fn emit_run(text: &str, marks: &Marks) -> String {
    if text.is_empty() {
        return String::new();
    }

    // Code is verbatim by definition, so it is never escaped and never
    // carries emphasis inside the span.
    let mut body = if marks.code {
        code_span(text)
    } else {
        escape_inline(text)
    };

    // Emphasis must hug the text: `** bold **` is not bold in CommonMark.
    // Leading and trailing spaces are lifted outside the markers.
    if marks.bold || marks.italic || marks.strike {
        let lead: String = body.chars().take_while(|c| *c == ' ').collect();
        let trail: String = body
            .chars()
            .rev()
            .take_while(|c| *c == ' ')
            .collect::<String>();
        let core = &body[lead.len()..body.len() - trail.len()];
        if core.is_empty() {
            return body;
        }
        let mut wrapped = core.to_string();
        if marks.strike {
            wrapped = format!("~~{wrapped}~~");
        }
        if marks.bold {
            wrapped = format!("**{wrapped}**");
        }
        if marks.italic {
            wrapped = format!("_{wrapped}_");
        }
        body = format!("{lead}{wrapped}{trail}");
    }

    if let Some(title) = &marks.wikilink {
        // A wiki link's target is a title, not escaped text: the renderer
        // resolves it by title (ADR-0001).
        body = format!("[[{title}]]");
    } else if let Some(url) = &marks.link {
        body = format!("[{body}]({})", escape_url(url));
    }

    body
}

/// Make a URL safe to sit inside `( )` without terminating the link early.
pub fn escape_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        match c {
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            ' ' => out.push_str("%20"),
            '<' | '>' | '"' | '\\' => {
                out.push('%');
                out.push_str(&format!("{:02X}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_that_looks_like_markup_is_escaped() {
        assert_eq!(escape_inline("2 * 3 * 4"), "2 \\* 3 \\* 4");
        assert_eq!(escape_inline("snake_case_name"), "snake\\_case\\_name");
        assert_eq!(
            escape_inline("see [1] and <tag>"),
            "see \\[1\\] and \\<tag\\>"
        );
    }

    #[test]
    fn line_leading_block_markers_are_escaped() {
        assert_eq!(escape_line_start("# not a heading"), "\\# not a heading");
        assert_eq!(escape_line_start("- not a list"), "\\- not a list");
        assert_eq!(escape_line_start("1. not a list"), "\\1. not a list");
        assert_eq!(escape_line_start("> not a quote"), "\\> not a quote");
        assert_eq!(escape_line_start("ordinary text"), "ordinary text");
        // A digit that is not a list marker stays untouched.
        assert_eq!(escape_line_start("1984 was a year"), "1984 was a year");
    }

    #[test]
    fn code_spans_survive_backticks_in_the_content() {
        assert_eq!(code_span("plain"), "`plain`");
        assert_eq!(code_span("has ` tick"), "``has ` tick``");
        assert_eq!(code_span("`lead"), "`` `lead ``");
    }

    #[test]
    fn emphasis_hugs_its_text() {
        let bold = Marks {
            bold: true,
            ..Default::default()
        };
        assert_eq!(emit_run(" word ", &bold), " **word** ");
        assert_eq!(emit_run("word", &bold), "**word**");
    }

    #[test]
    fn a_code_run_is_never_escaped() {
        let code = Marks {
            code: true,
            ..Default::default()
        };
        assert_eq!(emit_run("a_b*c", &code), "`a_b*c`");
    }

    #[test]
    fn links_escape_their_target() {
        let link = Marks {
            link: Some("http://example.com/a(b)".into()),
            ..Default::default()
        };
        assert_eq!(
            emit_run("text", &link),
            "[text](http://example.com/a%28b%29)"
        );
    }
}
