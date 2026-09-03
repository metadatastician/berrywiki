// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! What the importer tells you it is about to do, or just did.
//!
//! Two renderings of one [`ImportModel`]: AsciiDoc for a person (the repo's
//! documentation language) and JSON for a program. Both are pure functions of
//! the model, so a dry run and the real thing produce the same report for the
//! same notebook, and a test can compare them byte for byte.
//!
//! **Bodies are never in a report.** Only their size is. Two reasons, and the
//! second is the one that matters: a report is the thing a user pastes into a
//! bug tracker or hands to somebody helping them, and a personal notebook is
//! exactly the kind of thing whose contents should not travel by accident. A
//! byte count answers "did it come through" without disclosing what it said.
//!
//! Titles *are* included, because a lossiness report that will not tell you
//! which page lost something is not worth reading. That is a deliberate,
//! narrow disclosure and it is stated here so nobody has to infer it.
//!
//! Both renderings escape their content rather than trusting it. A node title
//! comes from a user's notebook and may contain a pipe, a quote, a brace or a
//! control character; a report that let those through would either corrupt its
//! own structure or, worse, silently drop characters while looking fine.

use crate::model::ImportModel;
use berrywiki_core::Severity;

/// Whether the run wrote anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Nothing was written; this is what *would* happen.
    DryRun,
    /// The pages in the model were written to the wiki.
    Applied,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::DryRun => "dry-run",
            Mode::Applied => "applied",
        }
    }
}

/// The facts about a run that are not in the model itself.
#[derive(Debug, Clone)]
pub struct Run<'a> {
    /// The source file's name, without any directory part. A report should
    /// not carry a path from someone's home directory.
    pub source: &'a str,
    pub mode: Mode,
}

// ---------------------------------------------------------------------------
// AsciiDoc
// ---------------------------------------------------------------------------

/// Escape text for inline AsciiDoc, preserving every character visually.
///
/// AsciiDoc honours a backslash escape for its inline markers, so a title of
/// `a*b*c` survives as `a*b*c` rather than rendering as `abc` with a bold `b`.
/// That distinction is the whole point: this is a report about what was lost,
/// and it may not lose anything itself.
fn adoc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            // Backslash first, or the escapes below escape each other.
            '\\' | '*' | '_' | '`' | '#' | '^' | '~' | '{' | '}' | '<' | '>' | '+' | '|' => {
                out.push('\\');
                out.push(c);
            }
            // A newline inside a title would end the list item or table row.
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// The human report.
pub fn asciidoc(model: &ImportModel, run: &Run<'_>) -> String {
    let mut s = String::new();

    s.push_str("= Import report\n");
    s.push_str(":source: ");
    s.push_str(&adoc(run.source));
    s.push('\n');
    s.push_str(":mode: ");
    s.push_str(run.mode.as_str());
    s.push_str("\n\n");

    match run.mode {
        Mode::DryRun => s.push_str(
            "This was a dry run. Nothing has been written. \
             Re-run with `--apply` to import.\n\n",
        ),
        Mode::Applied => s.push_str("These pages were written to the wiki.\n\n"),
    }

    s.push_str("== Summary\n\n");
    s.push_str("[cols=\"1,3\"]\n|===\n");
    s.push_str("| Source | ");
    s.push_str(&adoc(run.source));
    s.push('\n');
    s.push_str("| Source SHA-256 | `");
    s.push_str(&model.source_hash);
    s.push_str("`\n");
    s.push_str(&format!("| Pages | {}\n", model.nodes.len()));
    s.push_str(&format!("| Attachments | {}\n", model.asset_count()));
    s.push_str(&format!("| Deepest nesting | {}\n", model.depth()));
    s.push_str(&format!("| Diagnostics | {}\n", model.diagnostics.len()));
    s.push_str("|===\n\n");

    // Lossiness first, because it is the reason the report exists.
    s.push_str("== What was not carried across\n\n");
    let loss = model.loss_summary();
    if loss.is_empty() {
        s.push_str("Nothing was reported lost or changed.\n\n");
    } else {
        s.push_str(
            "Each row is a kind of difference between the source notebook and \
             the imported pages. A count is how many times it happened, not how \
             serious it is.\n\n",
        );
        s.push_str("[cols=\"3,1\"]\n|===\n| Code | Count\n\n");
        for (code, count) in &loss {
            s.push_str("| `");
            s.push_str(&adoc(code));
            s.push_str(&format!("` | {count}\n"));
        }
        s.push_str("|===\n\n");
    }

    let collisions = model.title_collisions();
    if !collisions.is_empty() {
        s.push_str("== Sibling titles that collide\n\n");
        s.push_str(
            "A page's filename is built from its title and its ancestors' \
             titles, so two siblings with the same title would land on the same \
             file. These need renaming, in the notebook or afterwards.\n\n",
        );
        for t in &collisions {
            s.push_str("* ");
            s.push_str(&adoc(t));
            s.push('\n');
        }
        s.push('\n');
    }

    s.push_str("== Pages\n\n");
    if model.nodes.is_empty() {
        s.push_str("None. The source held no readable nodes.\n\n");
    } else {
        for n in &model.nodes {
            let mut depth = 1;
            let mut cur = n.parent;
            while let Some(p) = cur {
                depth += 1;
                cur = model.nodes[p].parent;
            }
            for _ in 0..depth {
                s.push('*');
            }
            s.push(' ');
            s.push_str(&adoc(&n.title));
            s.push_str(&format!(" ({} bytes", n.body.len()));
            if !n.assets.is_empty() {
                s.push_str(&format!(", {} attachment", n.assets.len()));
                if n.assets.len() != 1 {
                    s.push('s');
                }
            }
            if !n.tags.is_empty() {
                s.push_str(", tags: ");
                s.push_str(&adoc(&n.tags.join(" ")));
            }
            s.push_str(")\n");
        }
        s.push('\n');
    }

    if !model.diagnostics.is_empty() {
        s.push_str("== Diagnostics\n\n");
        s.push_str("[cols=\"1,2,4,2\"]\n|===\n| Severity | Code | Message | Page\n\n");
        for d in &model.diagnostics {
            s.push_str(&format!("| {}\n", d.severity));
            s.push_str("| `");
            s.push_str(&adoc(&d.code));
            s.push_str("`\n| ");
            s.push_str(&adoc(&d.message));
            s.push_str("\n| ");
            match &d.page {
                Some(p) => s.push_str(&adoc(p)),
                None => s.push('-'),
            }
            s.push('\n');
        }
        s.push_str("|===\n");
    }

    s
}

// ---------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------

/// Escape a string for RFC 8259, minimally but completely.
///
/// Every control character below 0x20 must be escaped or the document is not
/// JSON. A CherryTree node title can contain one, so this is not theoretical.
fn json_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn indent(out: &mut String, level: usize) {
    for _ in 0..level * 2 {
        out.push(' ');
    }
}

fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

/// The machine report: canonical, 2-space indent, LF, fixed key order.
///
/// Hand-rolled for the reason in [`crate::hash`]: one emitter for one shape is
/// a better trade than a serialisation framework in a workspace whose only
/// third-party dependency is `comrak`.
pub fn json(model: &ImportModel, run: &Run<'_>) -> String {
    let mut s = String::new();
    s.push_str("{\n");

    indent(&mut s, 1);
    s.push_str("\"berrywiki_import\": 1,\n");

    indent(&mut s, 1);
    s.push_str("\"source\": ");
    json_string(run.source, &mut s);
    s.push_str(",\n");

    indent(&mut s, 1);
    s.push_str("\"source_sha256\": ");
    json_string(&model.source_hash, &mut s);
    s.push_str(",\n");

    indent(&mut s, 1);
    s.push_str("\"mode\": ");
    json_string(run.mode.as_str(), &mut s);
    s.push_str(",\n");

    indent(&mut s, 1);
    s.push_str(&format!("\"pages\": {},\n", model.nodes.len()));
    indent(&mut s, 1);
    s.push_str(&format!("\"attachments\": {},\n", model.asset_count()));
    indent(&mut s, 1);
    s.push_str(&format!("\"depth\": {},\n", model.depth()));

    // title_collisions
    indent(&mut s, 1);
    s.push_str("\"title_collisions\": ");
    let collisions = model.title_collisions();
    if collisions.is_empty() {
        s.push_str("[],\n");
    } else {
        s.push_str("[\n");
        for (i, t) in collisions.iter().enumerate() {
            indent(&mut s, 2);
            json_string(t, &mut s);
            s.push_str(if i + 1 == collisions.len() {
                "\n"
            } else {
                ",\n"
            });
        }
        indent(&mut s, 1);
        s.push_str("],\n");
    }

    // loss_summary
    indent(&mut s, 1);
    s.push_str("\"loss_summary\": ");
    let loss = model.loss_summary();
    if loss.is_empty() {
        s.push_str("[],\n");
    } else {
        s.push_str("[\n");
        for (i, (code, count)) in loss.iter().enumerate() {
            indent(&mut s, 2);
            s.push_str("{ \"code\": ");
            json_string(code, &mut s);
            s.push_str(&format!(", \"count\": {count} }}"));
            s.push_str(if i + 1 == loss.len() { "\n" } else { ",\n" });
        }
        indent(&mut s, 1);
        s.push_str("],\n");
    }

    // diagnostics
    indent(&mut s, 1);
    s.push_str("\"diagnostics\": ");
    if model.diagnostics.is_empty() {
        s.push_str("[],\n");
    } else {
        s.push_str("[\n");
        for (i, d) in model.diagnostics.iter().enumerate() {
            indent(&mut s, 2);
            s.push_str("{\n");
            indent(&mut s, 3);
            s.push_str("\"severity\": ");
            json_string(severity_str(d.severity), &mut s);
            s.push_str(",\n");
            indent(&mut s, 3);
            s.push_str("\"code\": ");
            json_string(&d.code, &mut s);
            s.push_str(",\n");
            indent(&mut s, 3);
            s.push_str("\"message\": ");
            json_string(&d.message, &mut s);
            s.push_str(",\n");
            indent(&mut s, 3);
            s.push_str("\"page\": ");
            match &d.page {
                Some(p) => json_string(p, &mut s),
                None => s.push_str("null"),
            }
            s.push('\n');
            indent(&mut s, 2);
            s.push('}');
            s.push_str(if i + 1 == model.diagnostics.len() {
                "\n"
            } else {
                ",\n"
            });
        }
        indent(&mut s, 1);
        s.push_str("],\n");
    }

    // nodes
    indent(&mut s, 1);
    s.push_str("\"nodes\": ");
    if model.nodes.is_empty() {
        s.push_str("[]\n");
    } else {
        s.push_str("[\n");
        for (i, n) in model.nodes.iter().enumerate() {
            indent(&mut s, 2);
            s.push_str("{\n");

            indent(&mut s, 3);
            s.push_str("\"id\": ");
            json_string(&n.id, &mut s);
            s.push_str(",\n");

            indent(&mut s, 3);
            s.push_str("\"source_id\": ");
            json_string(&n.source_id, &mut s);
            s.push_str(",\n");

            indent(&mut s, 3);
            s.push_str("\"parent\": ");
            match n.parent {
                Some(p) => s.push_str(&p.to_string()),
                None => s.push_str("null"),
            }
            s.push_str(",\n");

            indent(&mut s, 3);
            s.push_str(&format!("\"position\": {},\n", n.position));

            indent(&mut s, 3);
            s.push_str("\"title\": ");
            json_string(&n.title, &mut s);
            s.push_str(",\n");

            indent(&mut s, 3);
            s.push_str("\"tags\": ");
            if n.tags.is_empty() {
                s.push_str("[]");
            } else {
                s.push('[');
                for (j, t) in n.tags.iter().enumerate() {
                    if j > 0 {
                        s.push_str(", ");
                    }
                    json_string(t, &mut s);
                }
                s.push(']');
            }
            s.push_str(",\n");

            indent(&mut s, 3);
            s.push_str("\"marker\": ");
            json_string(&n.marker, &mut s);
            s.push_str(",\n");

            // The body itself is deliberately absent; see the module doc.
            indent(&mut s, 3);
            s.push_str(&format!("\"body_bytes\": {},\n", n.body.len()));

            indent(&mut s, 3);
            s.push_str("\"assets\": ");
            if n.assets.is_empty() {
                s.push_str("[]\n");
            } else {
                s.push_str("[\n");
                for (j, a) in n.assets.iter().enumerate() {
                    indent(&mut s, 4);
                    s.push_str("{ \"filename\": ");
                    json_string(&a.filename, &mut s);
                    s.push_str(&format!(", \"bytes\": {} }}", a.bytes.len()));
                    s.push_str(if j + 1 == n.assets.len() { "\n" } else { ",\n" });
                }
                indent(&mut s, 3);
                s.push_str("]\n");
            }

            indent(&mut s, 2);
            s.push('}');
            s.push_str(if i + 1 == model.nodes.len() {
                "\n"
            } else {
                ",\n"
            });
        }
        indent(&mut s, 1);
        s.push_str("]\n");
    }

    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ImportAsset, ImportNode};
    use berrywiki_core::Diagnostic;

    fn node(id: &str, title: &str, parent: Option<usize>, body: &str) -> ImportNode {
        ImportNode {
            id: id.to_string(),
            source_id: id.to_string(),
            parent,
            position: 0,
            title: title.to_string(),
            tags: Vec::new(),
            body: body.to_string(),
            assets: Vec::new(),
            marker: format!("cherrytree:abc:{id}"),
        }
    }

    fn sample() -> ImportModel {
        let mut m = ImportModel {
            source_hash: "a".repeat(64),
            ..Default::default()
        };
        m.nodes.push(node("1", "Root", None, "hello"));
        let mut child = node("2", "Child", Some(0), "world and more");
        child.tags = vec!["alpha".into(), "beta".into()];
        child.assets.push(ImportAsset {
            filename: "shot.png".into(),
            bytes: vec![0u8; 10],
        });
        m.nodes.push(child);
        m.nodes.push(node("3", "Deep", Some(1), ""));
        m.diagnostics
            .push(Diagnostic::warning("import.ct.colour-dropped", "colour lost").with_page("2"));
        m.diagnostics.push(Diagnostic::warning(
            "import.ct.colour-dropped",
            "colour lost again",
        ));
        m.diagnostics.push(Diagnostic::new(
            Severity::Info,
            "import.ct.anchor-dropped",
            "an anchor went",
        ));
        m
    }

    /// A title built to break a report that trusts its input.
    fn hostile() -> ImportModel {
        let mut m = ImportModel {
            source_hash: "b".repeat(64),
            ..Default::default()
        };
        m.nodes.push(node(
            "1",
            "a*b*c | d \"e\" \\f {g} <h> `i` +j+ _k_ #l# ~m~ ^n^",
            None,
            "x",
        ));
        m.nodes
            .push(node("2", "line\nbreak\tand\u{1}control", Some(0), "y"));
        m
    }

    #[test]
    fn a_report_never_contains_a_body() {
        // The module promises this. A change that starts emitting bodies is a
        // privacy regression, not a formatting one, so it is tested by
        // content and not by shape.
        let mut m = sample();
        let secret = "correct-horse-battery-staple";
        m.nodes[0].body = format!("some notes with {secret} inside");
        let run = Run {
            source: "notes.ctd",
            mode: Mode::DryRun,
        };
        let a = asciidoc(&m, &run);
        let j = json(&m, &run);
        assert!(!a.contains(secret), "AsciiDoc report leaked a body");
        assert!(!j.contains(secret), "JSON report leaked a body");
        // But the size is reported, so "did it come through" is answerable.
        assert!(j.contains(&format!("\"body_bytes\": {}", m.nodes[0].body.len())));
    }

    #[test]
    fn asciidoc_states_the_mode_and_does_not_promise_a_write_it_did_not_do() {
        let m = sample();
        let dry = asciidoc(
            &m,
            &Run {
                source: "n.ctd",
                mode: Mode::DryRun,
            },
        );
        assert!(dry.contains("Nothing has been written"), "{dry}");
        assert!(dry.contains(":mode: dry-run"));
        let applied = asciidoc(
            &m,
            &Run {
                source: "n.ctd",
                mode: Mode::Applied,
            },
        );
        assert!(applied.contains("were written to the wiki"));
        assert!(!applied.contains("Nothing has been written"));
    }

    #[test]
    fn asciidoc_carries_the_counts_and_the_loss_summary() {
        let m = sample();
        let a = asciidoc(
            &m,
            &Run {
                source: "n.ctd",
                mode: Mode::DryRun,
            },
        );
        assert!(a.contains("| Pages | 3"), "{a}");
        assert!(a.contains("| Attachments | 1"), "{a}");
        assert!(a.contains("| Deepest nesting | 3"), "{a}");
        // Two of one code and one of another, aggregated.
        assert!(a.contains("`import.ct.colour-dropped` | 2"), "{a}");
        assert!(a.contains("`import.ct.anchor-dropped` | 1"), "{a}");
        // The tree is rendered by depth.
        assert!(a.contains("* Root"), "{a}");
        assert!(a.contains("** Child"), "{a}");
        assert!(a.contains("*** Deep"), "{a}");
    }

    #[test]
    fn asciidoc_escapes_a_title_without_losing_a_character() {
        // The failure this guards against is silent: `a*b*c` rendered raw
        // becomes "abc" with a bold b, so characters vanish from a report
        // whose entire job is to say what vanished.
        let m = hostile();
        let a = asciidoc(
            &m,
            &Run {
                source: "n.ctd",
                mode: Mode::DryRun,
            },
        );
        assert!(a.contains(r"a\*b\*c"), "asterisks unescaped: {a}");
        assert!(a.contains(r"\|"), "pipe unescaped, table would break: {a}");
        assert!(a.contains(r"\{g\}"), "braces unescaped: {a}");
        assert!(a.contains(r"\`i\`"), "backticks unescaped: {a}");
        // A newline in a title must not end the list item.
        let lines: Vec<&str> = a.lines().filter(|l| l.contains("break")).collect();
        assert_eq!(lines.len(), 1, "a title's newline split a row: {lines:?}");
        assert!(lines[0].contains("line break"), "{}", lines[0]);
    }

    #[test]
    fn json_escapes_every_control_character() {
        let m = hostile();
        let j = json(
            &m,
            &Run {
                source: "n.ctd",
                mode: Mode::DryRun,
            },
        );
        assert!(j.contains(r"\n"), "newline unescaped");
        assert!(j.contains(r"\t"), "tab unescaped");
        assert!(j.contains(r"\u0001"), "control char unescaped: {j}");
        assert!(j.contains(r#"\"e\""#), "quote unescaped: {j}");
        assert!(j.contains(r"\\f"), "backslash unescaped: {j}");
        // No raw control byte survives anywhere in the document.
        assert!(
            !j.chars().any(|c| (c as u32) < 0x20 && c != '\n'),
            "a raw control character reached the JSON"
        );
    }

    #[test]
    fn json_is_well_formed_and_balanced() {
        // Without a JSON parser in the workspace, check the properties that a
        // hand-rolled emitter actually gets wrong: unbalanced brackets and a
        // trailing comma before a close.
        for m in [sample(), hostile(), ImportModel::default()] {
            let j = json(
                &m,
                &Run {
                    source: "n.ctd",
                    mode: Mode::Applied,
                },
            );
            let mut depth = 0i32;
            let mut in_string = false;
            let mut escaped = false;
            for c in j.chars() {
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        in_string = false;
                    }
                    continue;
                }
                match c {
                    '"' => in_string = true,
                    '{' | '[' => depth += 1,
                    '}' | ']' => depth -= 1,
                    _ => {}
                }
                assert!(depth >= 0, "closed more than was opened:\n{j}");
            }
            assert_eq!(depth, 0, "unbalanced brackets:\n{j}");
            assert!(!in_string, "unterminated string:\n{j}");
            for bad in [",\n  }", ",\n  ]", ",}", ",]", ", }", ", ]"] {
                assert!(!j.contains(bad), "trailing comma before {bad:?}:\n{j}");
            }
            assert!(j.ends_with("}\n"), "should end with a closed object:\n{j}");
        }
    }

    #[test]
    fn json_carries_the_shape_a_program_needs() {
        let m = sample();
        let j = json(
            &m,
            &Run {
                source: "n.ctd",
                mode: Mode::Applied,
            },
        );
        assert!(j.contains("\"berrywiki_import\": 1"), "no format version");
        assert!(j.contains("\"mode\": \"applied\""));
        assert!(j.contains(&format!("\"source_sha256\": \"{}\"", "a".repeat(64))));
        assert!(j.contains("\"pages\": 3"));
        assert!(j.contains("\"attachments\": 1"));
        assert!(j.contains("\"parent\": null"), "a root must say null");
        assert!(j.contains("\"parent\": 0"), "a child must name its index");
        assert!(j.contains("\"tags\": [\"alpha\", \"beta\"]"), "{j}");
        assert!(
            j.contains("\"filename\": \"shot.png\", \"bytes\": 10"),
            "{j}"
        );
        assert!(j.contains("\"count\": 2"), "loss summary not aggregated");
    }

    #[test]
    fn an_empty_model_reports_emptily_rather_than_wrongly() {
        let m = ImportModel::default();
        let run = Run {
            source: "empty.ctd",
            mode: Mode::DryRun,
        };
        let a = asciidoc(&m, &run);
        assert!(a.contains("| Pages | 0"), "{a}");
        assert!(a.contains("Nothing was reported lost"), "{a}");
        assert!(
            a.contains("None. The source held no readable nodes."),
            "{a}"
        );
        let j = json(&m, &run);
        assert!(j.contains("\"pages\": 0"));
        assert!(j.contains("\"nodes\": []"));
        assert!(j.contains("\"diagnostics\": []"));
    }

    #[test]
    fn both_reports_are_deterministic() {
        // The same model must give the same bytes, or a report cannot be
        // diffed between two runs to show what changed in the notebook.
        let m = sample();
        let run = Run {
            source: "n.ctd",
            mode: Mode::DryRun,
        };
        assert_eq!(asciidoc(&m, &run), asciidoc(&m, &run));
        assert_eq!(json(&m, &run), json(&m, &run));
    }

    #[test]
    fn the_source_name_is_escaped_too() {
        // A filename is attacker-controlled in exactly the same way a title
        // is, and it lands in an attribute line and a table cell.
        let m = ImportModel::default();
        let run = Run {
            source: "we|ird\"name*.ctd",
            mode: Mode::DryRun,
        };
        let a = asciidoc(&m, &run);
        assert!(a.contains(r"we\|ird"), "{a}");
        assert!(a.contains(r"name\*"), "{a}");
        let j = json(&m, &run);
        assert!(j.contains(r#"\"name"#), "{j}");
    }
}
