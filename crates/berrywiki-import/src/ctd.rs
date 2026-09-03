// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! Reading a CherryTree `.ctd` notebook into the neutral import model.
//!
//! # The shape of the file
//!
//! A `.ctd` is XML. `<cherrytree>` holds `<node>` elements nested to give the
//! tree. A node's own content is its direct children that are not `<node>`:
//! a sequence of `<rich_text>` runs carrying the text and its styling, then
//! `<codebox>`, `<table>` and `<encoded_png>` elements each carrying a
//! `char_offset` saying where in that text they belong.
//!
//! # The trap in `char_offset`
//!
//! The offset counts **characters, not bytes**. An implementation that
//! indexes the concatenated runs by byte gives identical output for ASCII
//! notebooks and silently misplaces every widget in a notebook containing a
//! single accented letter before an image. The fixtures therefore include a
//! non-ASCII run before an image, and that test is the reason this module
//! counts with `chars()` throughout.
//!
//! # Two passes
//!
//! `link="node 7"` names a node by CherryTree's id, but a BerryWiki link names
//! a page by title (ADR-0001), and the target may not have been read yet. So
//! parsing is split: pass one walks the XML and mints an id and title for
//! every node; pass two renders each node's Markdown with the complete
//! id-to-title map in hand. No sentinel is ever placed in the text, which
//! means no sentinel can be spoofed by the notebook's own content.
//!
//! # What is not read
//!
//! `.ctb` (SQLite), `.ctz` and `.ctx` (7-zip archives of the other two) are
//! not this module's business. `crate::refuse_by_extension` names them and
//! says so, rather than letting a binary file reach an XML parser and produce
//! a confusing error about a malformed tag.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use berrywiki_core::{Diagnostic, Severity};

use crate::b64;
use crate::markdown::{emit_run, escape_inline, escape_line_start, Marks};
use crate::marker::{cherrytree_marker, page_id};
use crate::model::{ImportAsset, ImportModel, ImportNode};
use crate::xml::{Event, Reader};

/// Where a `link=` attribute points.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkTarget {
    /// `link="webs https://…"`.
    Web(String),
    /// `link="node 7"`, optionally with a trailing anchor name.
    Node(String),
    /// `link="file …"` or `link="fold …"`: a path on the machine that wrote
    /// the notebook, which means nothing in a wiki.
    LocalPath,
    /// Something this reader does not recognise.
    Unknown,
}

/// Styling carried by one `<rich_text>` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RunStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    monospace: bool,
    heading: Option<u8>,
    link: Option<LinkTarget>,
}

/// A non-text element anchored into a node's text by `char_offset`.
#[derive(Debug, Clone)]
enum Widget {
    Codebox {
        lang: String,
        code: String,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
    Image {
        filename: Option<String>,
        data: String,
    },
    EmbeddedFile {
        filename: String,
        data: String,
    },
    Anchor {
        name: String,
    },
}

/// One text-or-widget piece of a node's content, in reading order.
enum Piece {
    Text(String, RunStyle),
    Widget(Widget),
}

/// What the parser is currently collecting characters into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    None,
    RichText,
    Codebox,
    Png,
    Cell,
}

/// A node under construction during pass one.
struct Build {
    node_id: String,
    title: String,
    tags: Vec<String>,
    prog_lang: String,
    parent: Option<usize>,
    position: i64,
    runs: Vec<(String, RunStyle)>,
    widgets: Vec<(usize, Widget)>,
    /// Diagnostics raised while reading the node's own attributes.
    early: Vec<Diagnostic>,
}

/// Read a `.ctd` document.
///
/// `source_hash` is the hex SHA-256 of the file as read; it becomes part of
/// every page's `source:` marker. Never panics: a malformed document produces
/// an `ImportModel` carrying diagnostics and whatever nodes were readable.
pub fn parse_ctd(src: &str, source_hash: &str) -> ImportModel {
    let mut model = ImportModel {
        source_hash: source_hash.to_string(),
        ..ImportModel::default()
    };

    let (builds, mut diagnostics) = match walk(src) {
        Ok(pair) => pair,
        Err(d) => {
            model.diagnostics.push(d);
            return model;
        }
    };

    // Pass one: identity. Every node gets its id and its title before any
    // body is rendered, so a link may point forwards as freely as backwards.
    let mut titles: BTreeMap<String, String> = BTreeMap::new();
    let mut ids: Vec<String> = Vec::with_capacity(builds.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for b in &builds {
        if !seen.insert(b.node_id.clone()) {
            diagnostics.push(Diagnostic::warning(
                "import.ct.duplicate-node-id",
                format!(
                    "Two nodes share the CherryTree id {}; the later one keeps its own page \
                     but links to it may reach the earlier.",
                    b.node_id
                ),
            ));
        }
        let marker = cherrytree_marker(source_hash, &b.node_id);
        ids.push(page_id(&marker));
        titles
            .entry(b.node_id.clone())
            .or_insert_with(|| b.title.clone());
    }

    // Pass two: bodies, now that every link has somewhere to land.
    for (i, b) in builds.into_iter().enumerate() {
        let id = ids[i].clone();
        let marker = cherrytree_marker(source_hash, &b.node_id);
        let mut node_diags = b.early.clone();

        let (body, assets) = render_node(&b, &id, &titles, &mut node_diags);

        for d in node_diags {
            diagnostics.push(d.with_page(id.clone()));
        }

        model.nodes.push(ImportNode {
            id,
            source_id: b.node_id,
            parent: b.parent,
            position: b.position,
            title: b.title,
            tags: b.tags,
            body,
            assets,
            marker,
        });
    }

    model.diagnostics = diagnostics;
    model
}

/// Pass one proper: walk the XML into `Build`s in pre-order.
#[allow(clippy::type_complexity)]
fn walk(src: &str) -> Result<(Vec<Build>, Vec<Diagnostic>), Diagnostic> {
    let mut reader = Reader::new(src);
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut builds: Vec<Build> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut saw_root = false;

    let mut capture = Capture::None;
    let mut buf = String::new();
    let mut pending_style = RunStyle::default();
    let mut pending_offset: usize = 0;
    let mut pending_lang = String::new();
    let mut pending_png_filename: Option<String> = None;
    let mut pending_png_is_file = false;
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_row: Vec<String> = Vec::new();
    let mut sibling_count: Vec<i64> = vec![0];

    loop {
        let event = match reader.next_event() {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(e) => {
                // A malformed document stops here rather than guessing. What
                // was read before the fault is still returned.
                diagnostics.push(Diagnostic::error(
                    e.code,
                    format!("{} (at byte {})", e.message, e.offset),
                ));
                break;
            }
        };

        match event {
            Event::Start {
                ref name,
                self_closing,
                ..
            } => {
                match name.as_str() {
                    "cherrytree" => saw_root = true,
                    "node" => {
                        if !saw_root {
                            return Err(Diagnostic::error(
                                "import.ct.not-cherrytree",
                                "This XML does not start with a <cherrytree> element, so it is \
                                 not a CherryTree .ctd document. Nothing was imported.",
                            ));
                        }
                        let index = builds.len();
                        let parent = stack.last().copied();
                        let depth = stack.len();
                        if sibling_count.len() <= depth {
                            sibling_count.resize(depth + 1, 0);
                        }
                        sibling_count.truncate(depth + 1);
                        let position = sibling_count[depth];
                        sibling_count[depth] += 1;
                        sibling_count.push(0);

                        let mut early = Vec::new();
                        let node_id = event
                            .attr("unique_id")
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("anon-{index}"));
                        let raw_title = event.attr("name").unwrap_or("").trim().to_string();
                        let title = if raw_title.is_empty() {
                            early.push(Diagnostic::new(
                                Severity::Info,
                                "import.ct.untitled-node",
                                "A node had no name; it was titled \"Untitled\".",
                            ));
                            "Untitled".to_string()
                        } else {
                            raw_title
                        };
                        let tags: Vec<String> = event
                            .attr("tags")
                            .unwrap_or("")
                            .split_whitespace()
                            .map(str::to_string)
                            .collect();
                        let prog_lang = event
                            .attr("prog_lang")
                            .unwrap_or("custom-colors")
                            .to_string();

                        for (attr, what) in [
                            ("readonly", "a read-only flag"),
                            ("custom_icon_id", "a custom icon"),
                            ("is_bold", "a bold tree label"),
                            ("foreground", "a coloured tree label"),
                        ] {
                            let v = event.attr(attr).unwrap_or("");
                            if !v.is_empty() && v != "0" && v != "False" {
                                early.push(Diagnostic::new(
                                    Severity::Info,
                                    "import.ct.node-attribute-dropped",
                                    format!(
                                        "The node carried {what} ({attr}), which BerryWiki has no \
                                         equivalent for. The content is unaffected."
                                    ),
                                ));
                            }
                        }

                        builds.push(Build {
                            node_id,
                            title,
                            tags,
                            prog_lang,
                            parent,
                            position,
                            runs: Vec::new(),
                            widgets: Vec::new(),
                            early,
                        });
                        if !self_closing {
                            stack.push(index);
                        }
                    }
                    "rich_text" => {
                        pending_style = read_style(&event, &mut diagnostics);
                        buf.clear();
                        capture = Capture::RichText;
                    }
                    "codebox" => {
                        pending_offset = offset_of(&event);
                        pending_lang = event
                            .attr("syntax_highlighting")
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        buf.clear();
                        capture = Capture::Codebox;
                    }
                    "encoded_png" => {
                        pending_offset = offset_of(&event);
                        // Three different things wear this element's name.
                        if let Some(anchor) = event.attr("anchor").filter(|s| !s.is_empty()) {
                            push_widget(
                                &mut builds,
                                &stack,
                                pending_offset,
                                Widget::Anchor {
                                    name: anchor.to_string(),
                                },
                            );
                            capture = Capture::None;
                        } else {
                            pending_png_filename = event
                                .attr("filename")
                                .filter(|s| !s.is_empty())
                                .map(str::to_string);
                            pending_png_is_file = pending_png_filename.is_some();
                            buf.clear();
                            capture = Capture::Png;
                        }
                    }
                    "table" => {
                        pending_offset = offset_of(&event);
                        table_rows.clear();
                        capture = Capture::None;
                    }
                    "row" => table_row.clear(),
                    "cell" => {
                        buf.clear();
                        capture = Capture::Cell;
                    }
                    _ => {}
                }
                if self_closing {
                    close_element(
                        name,
                        &mut builds,
                        &stack,
                        &mut capture,
                        &mut buf,
                        &pending_style,
                        pending_offset,
                        &pending_lang,
                        &pending_png_filename,
                        pending_png_is_file,
                        &mut table_rows,
                        &mut table_row,
                    );
                    if name == "node" {
                        // Already not pushed; nothing to pop.
                    }
                }
            }
            Event::End { ref name } => {
                if name == "node" {
                    stack.pop();
                    let depth = stack.len() + 1;
                    if sibling_count.len() > depth {
                        sibling_count.truncate(depth + 1);
                    }
                } else {
                    close_element(
                        name,
                        &mut builds,
                        &stack,
                        &mut capture,
                        &mut buf,
                        &pending_style,
                        pending_offset,
                        &pending_lang,
                        &pending_png_filename,
                        pending_png_is_file,
                        &mut table_rows,
                        &mut table_row,
                    );
                }
            }
            Event::Text(t) => {
                if capture != Capture::None {
                    buf.push_str(&t);
                }
            }
        }
    }

    for entity in &reader.unknown_entities {
        diagnostics.push(Diagnostic::warning(
            "import.xml-unknown-entity",
            format!(
                "The entity {entity} is not one of the five XML defines and was kept verbatim \
                 rather than guessed at."
            ),
        ));
    }

    if !saw_root {
        return Err(Diagnostic::error(
            "import.ct.not-cherrytree",
            "No <cherrytree> element was found, so this is not a CherryTree .ctd document. \
             Nothing was imported.",
        ));
    }

    Ok((builds, diagnostics))
}

#[allow(clippy::too_many_arguments)]
fn close_element(
    name: &str,
    builds: &mut [Build],
    stack: &[usize],
    capture: &mut Capture,
    buf: &mut String,
    pending_style: &RunStyle,
    pending_offset: usize,
    pending_lang: &str,
    pending_png_filename: &Option<String>,
    pending_png_is_file: bool,
    table_rows: &mut Vec<Vec<String>>,
    table_row: &mut Vec<String>,
) {
    match name {
        "rich_text" => {
            if *capture == Capture::RichText {
                if let Some(&i) = stack.last() {
                    if !buf.is_empty() {
                        builds[i].runs.push((buf.clone(), pending_style.clone()));
                    }
                }
            }
            buf.clear();
            *capture = Capture::None;
        }
        "codebox" => {
            if *capture == Capture::Codebox {
                push_widget(
                    builds,
                    stack,
                    pending_offset,
                    Widget::Codebox {
                        lang: pending_lang.to_string(),
                        code: buf.clone(),
                    },
                );
            }
            buf.clear();
            *capture = Capture::None;
        }
        "encoded_png" => {
            if *capture == Capture::Png {
                let widget = if pending_png_is_file {
                    Widget::EmbeddedFile {
                        filename: pending_png_filename.clone().unwrap_or_default(),
                        data: buf.clone(),
                    }
                } else {
                    Widget::Image {
                        filename: pending_png_filename.clone(),
                        data: buf.clone(),
                    }
                };
                push_widget(builds, stack, pending_offset, widget);
            }
            buf.clear();
            *capture = Capture::None;
        }
        "cell" => {
            if *capture == Capture::Cell {
                table_row.push(buf.clone());
            }
            buf.clear();
            *capture = Capture::None;
        }
        "row" => {
            table_rows.push(std::mem::take(table_row));
        }
        "table" => {
            push_widget(
                builds,
                stack,
                pending_offset,
                Widget::Table {
                    rows: std::mem::take(table_rows),
                },
            );
        }
        _ => {}
    }
}

fn push_widget(builds: &mut [Build], stack: &[usize], offset: usize, widget: Widget) {
    if let Some(&i) = stack.last() {
        builds[i].widgets.push((offset, widget));
    }
}

fn offset_of(event: &Event) -> usize {
    event
        .attr("char_offset")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(usize::MAX)
}

/// Read a `<rich_text>` element's attributes into a style, recording every
/// attribute BerryWiki has no home for.
fn read_style(event: &Event, diagnostics: &mut Vec<Diagnostic>) -> RunStyle {
    let mut style = RunStyle::default();
    let mut note = |code: &'static str, msg: String| {
        let d = Diagnostic::new(Severity::Info, code, msg);
        if !diagnostics.iter().any(|e| e.code == d.code) {
            diagnostics.push(d);
        }
    };

    if event.attr("weight") == Some("heavy") {
        style.bold = true;
    }
    if event.attr("style") == Some("italic") {
        style.italic = true;
    }
    if event.attr("strikethrough") == Some("true") {
        style.strike = true;
    }
    if event.attr("family") == Some("monospace") {
        style.monospace = true;
    }
    match event.attr("scale") {
        Some("h1") => style.heading = Some(1),
        Some("h2") => style.heading = Some(2),
        Some("h3") => style.heading = Some(3),
        Some("sup") | Some("sub") => note(
            "import.ct.superscript-dropped",
            "Superscript or subscript text was imported at normal size; Markdown has no \
             portable form for it."
                .into(),
        ),
        Some("small") => note(
            "import.ct.small-text-dropped",
            "Small text was imported at normal size.".into(),
        ),
        _ => {}
    }
    if event.attr("underline").is_some_and(|v| v != "none") {
        note(
            "import.ct.underline-dropped",
            "Underlined text was imported without the underline; Markdown has no underline \
             and GitHub strips the HTML that would give one."
                .into(),
        );
    }
    if event.attr("foreground").is_some() || event.attr("background").is_some() {
        note(
            "import.ct.colour-dropped",
            "Text colours and highlights were dropped; the text itself is unchanged.".into(),
        );
    }
    if event.attr("justification").is_some_and(|v| v != "left") {
        note(
            "import.ct.justification-dropped",
            "Centred or right-aligned text was imported left-aligned.".into(),
        );
    }
    if event.attr("indent").is_some_and(|v| v != "0") {
        note(
            "import.ct.indent-dropped",
            "Paragraph indentation was dropped.".into(),
        );
    }

    if let Some(link) = event.attr("link").filter(|s| !s.is_empty()) {
        style.link = Some(parse_link(link));
    }
    style
}

/// Read CherryTree's `link=` attribute, whose value is a space-separated
/// scheme and payload.
fn parse_link(value: &str) -> LinkTarget {
    let mut parts = value.splitn(2, ' ');
    match (parts.next(), parts.next()) {
        (Some("webs"), Some(url)) => LinkTarget::Web(url.trim().to_string()),
        (Some("node"), Some(rest)) => {
            // "node 7" or "node 7 anchor-name": the id is the first token.
            let id = rest.split_whitespace().next().unwrap_or("").to_string();
            if id.is_empty() {
                LinkTarget::Unknown
            } else {
                LinkTarget::Node(id)
            }
        }
        (Some("file"), Some(_)) | (Some("fold"), Some(_)) => LinkTarget::LocalPath,
        _ => LinkTarget::Unknown,
    }
}

// ---------------------------------------------------------------- pass two

/// Render one node's content to Markdown, collecting its assets.
fn render_node(
    build: &Build,
    id: &str,
    titles: &BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> (String, Vec<ImportAsset>) {
    // A node whose `prog_lang` is not the rich-text sentinel is a code node in
    // CherryTree: the whole thing is one program, and treating its runs as
    // prose would escape the source into nonsense.
    if build.prog_lang != "custom-colors" && !build.prog_lang.is_empty() {
        let code: String = build.runs.iter().map(|(t, _)| t.as_str()).collect();
        return (fenced(&code, &build.prog_lang), Vec::new());
    }

    let pieces = splice(build);
    let mut assets: Vec<ImportAsset> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut para = String::new();
    let mut image_n = 0usize;

    for piece in pieces {
        match piece {
            Piece::Text(text, style) => {
                let marks = resolve_marks(&style, titles, diagnostics);
                for (n, segment) in text.split('\n').enumerate() {
                    if n > 0 {
                        flush(&mut blocks, &mut para);
                    }
                    if segment.is_empty() {
                        continue;
                    }
                    if let Some(level) = style.heading {
                        flush(&mut blocks, &mut para);
                        blocks.push(format!(
                            "{} {}",
                            "#".repeat(level as usize),
                            emit_run(
                                segment,
                                &Marks {
                                    heading: None,
                                    ..marks.clone()
                                }
                            )
                        ));
                    } else {
                        let rendered = emit_run(segment, &marks);
                        if para.is_empty() {
                            para.push_str(&escape_line_start(&rendered));
                        } else {
                            para.push_str(&rendered);
                        }
                    }
                }
            }
            Piece::Widget(w) => {
                flush(&mut blocks, &mut para);
                render_widget(w, id, &mut image_n, &mut assets, &mut blocks, diagnostics);
            }
        }
    }
    flush(&mut blocks, &mut para);

    (blocks.join("\n\n"), assets)
}

fn flush(blocks: &mut Vec<String>, para: &mut String) {
    let text = para.trim_end();
    if !text.is_empty() {
        blocks.push(text.to_string());
    }
    para.clear();
}

/// Turn a run's style into emission marks, resolving any node link to a title.
fn resolve_marks(
    style: &RunStyle,
    titles: &BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Marks {
    let mut marks = Marks {
        bold: style.bold,
        italic: style.italic,
        strike: style.strike,
        code: style.monospace,
        heading: style.heading,
        link: None,
        wikilink: None,
    };
    let note = |diagnostics: &mut Vec<Diagnostic>, code: &'static str, msg: String| {
        let d = Diagnostic::new(Severity::Info, code, msg);
        if !diagnostics.iter().any(|e| e.code == d.code) {
            diagnostics.push(d);
        }
    };

    match &style.link {
        Some(LinkTarget::Web(url)) => marks.link = Some(url.clone()),
        Some(LinkTarget::Node(node_id)) => match titles.get(node_id) {
            Some(title) if !title.contains("]]") && !title.contains('[') => {
                marks.wikilink = Some(title.clone());
            }
            Some(_) => note(
                diagnostics,
                "import.ct.wikilink-title-unusable",
                "A link pointed at a node whose title contains square brackets, which a \
                 [[wiki link]] cannot carry. The text was kept without the link."
                    .into(),
            ),
            None => note(
                diagnostics,
                "import.ct.link-target-missing",
                format!(
                    "A link pointed at node {node_id}, which is not in this notebook. The text \
                     was kept without the link."
                ),
            ),
        },
        Some(LinkTarget::LocalPath) => note(
            diagnostics,
            "import.ct.link-to-file-dropped",
            "A link pointed at a file or folder on the machine that wrote the notebook. Such a \
             path means nothing in a wiki, so the text was kept without the link."
                .into(),
        ),
        Some(LinkTarget::Unknown) => note(
            diagnostics,
            "import.ct.link-target-unknown",
            "A link used a scheme this reader does not know. The text was kept without the link."
                .into(),
        ),
        None => {}
    }
    marks
}

/// Interleave a node's runs and its `char_offset`-anchored widgets.
///
/// Counts characters, never bytes: see the module documentation.
fn splice(build: &Build) -> Vec<Piece> {
    let total: usize = build.runs.iter().map(|(t, _)| t.chars().count()).sum();

    let mut widgets: Vec<(usize, Widget)> = build.widgets.clone();
    // `usize::MAX` is the "no char_offset given" marker; clamp it to the end
    // so such a widget lands after the text rather than being lost.
    for w in &mut widgets {
        if w.0 > total {
            w.0 = total;
        }
    }
    // A stable sort keeps two widgets at the same offset in document order.
    widgets.sort_by_key(|(o, _)| *o);
    let mut queue: VecDeque<(usize, Widget)> = widgets.into();

    let mut pieces: Vec<Piece> = Vec::new();
    let mut done = 0usize;

    for (text, style) in &build.runs {
        let mut rest: &str = text;
        while let Some(&(target, _)) = queue.front() {
            let rest_chars = rest.chars().count();
            if target > done + rest_chars {
                break;
            }
            let split = target.saturating_sub(done);
            let byte = rest
                .char_indices()
                .nth(split)
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            let (head, tail) = rest.split_at(byte);
            if !head.is_empty() {
                pieces.push(Piece::Text(head.to_string(), style.clone()));
            }
            done += split;
            rest = tail;
            let (_, widget) = queue.pop_front().expect("front was just observed");
            pieces.push(Piece::Widget(widget));
        }
        if !rest.is_empty() {
            done += rest.chars().count();
            pieces.push(Piece::Text(rest.to_string(), style.clone()));
        }
    }

    while let Some((_, widget)) = queue.pop_front() {
        pieces.push(Piece::Widget(widget));
    }
    pieces
}

fn render_widget(
    widget: Widget,
    page_id: &str,
    image_n: &mut usize,
    assets: &mut Vec<ImportAsset>,
    blocks: &mut Vec<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match widget {
        Widget::Codebox { lang, code } => blocks.push(fenced(&code, &lang)),
        Widget::Table { rows } => {
            if rows.is_empty() {
                return;
            }
            blocks.push(render_table(&rows, diagnostics));
        }
        Widget::Image { filename, data } => match b64::decode(&data) {
            Ok(bytes) if !bytes.is_empty() => {
                *image_n += 1;
                let name = filename.unwrap_or_else(|| format!("image-{image_n}.png"));
                let name = sanitise_filename(&name, "png");
                blocks.push(format!("![]({})", asset_href(page_id, &name)));
                assets.push(ImportAsset {
                    filename: name,
                    bytes,
                });
            }
            _ => diagnostics.push(Diagnostic::warning(
                "import.ct.image-decode-failed",
                "An embedded image could not be decoded and was left out. The surrounding text \
                 is unaffected.",
            )),
        },
        Widget::EmbeddedFile { filename, data } => match b64::decode(&data) {
            Ok(bytes) if !bytes.is_empty() => {
                let name = sanitise_filename(&filename, "bin");
                if !is_servable(&name) {
                    diagnostics.push(Diagnostic::warning(
                        "import.ct.embedded-file-not-servable",
                        format!(
                            "The embedded file {name} has an extension outside BerryWiki's \
                             attachment allowlist (ADR-0011), so it is stored in the repository \
                             and readable in a clone, but the /assets route will not serve it."
                        ),
                    ));
                }
                blocks.push(format!(
                    "[{}]({})",
                    escape_inline(&name),
                    asset_href(page_id, &name)
                ));
                assets.push(ImportAsset {
                    filename: name,
                    bytes,
                });
            }
            _ => diagnostics.push(Diagnostic::warning(
                "import.ct.embedded-file-decode-failed",
                format!("The embedded file {filename} could not be decoded and was left out."),
            )),
        },
        Widget::Anchor { name } => diagnostics.push(Diagnostic::new(
            Severity::Info,
            "import.ct.anchor-dropped",
            format!(
                "The anchor \"{name}\" was dropped. BerryWiki anchors are heading anchors, and \
                 inventing a heading to carry this one would change the page."
            ),
        )),
    }
}

/// Render a table, stating the assumption it rests on.
fn render_table(rows: &[Vec<String>], diagnostics: &mut Vec<Diagnostic>) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }

    // ASSUMPTION, not verified against a real notebook: the first `<row>` in
    // the file is the header row. If CherryTree writes the header last, every
    // imported table shows its headings as its final data row, which is
    // visible at a glance. The diagnostic tells the reader to look.
    diagnostics.push(Diagnostic::new(
        Severity::Info,
        "import.ct.table-header-assumed",
        "A table was imported with its first row as the header. Markdown tables must have one, \
         and which row CherryTree writes first is unverified. Check the table and swap the rows \
         if the headings ended up at the bottom.",
    ));

    let cell = |s: &str, diagnostics: &mut Vec<Diagnostic>| -> String {
        let flat = if s.contains('\n') {
            let d = Diagnostic::new(
                Severity::Info,
                "import.ct.table-newline-flattened",
                "A table cell held more than one line. Markdown table cells cannot, so the \
                 lines were joined with a space.",
            );
            if !diagnostics.iter().any(|e| e.code == d.code) {
                diagnostics.push(d);
            }
            s.replace('\n', " ")
        } else {
            s.to_string()
        };
        escape_inline(flat.trim())
    };

    let mut out = String::new();
    for (n, row) in rows.iter().enumerate() {
        out.push('|');
        for i in 0..width {
            let text = row.get(i).map(String::as_str).unwrap_or("");
            out.push(' ');
            out.push_str(&cell(text, diagnostics));
            out.push_str(" |");
        }
        out.push('\n');
        if n == 0 {
            out.push('|');
            for _ in 0..width {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Wrap text in a fence long enough that its own backticks cannot close it.
fn fenced(code: &str, lang: &str) -> String {
    let mut longest = 0usize;
    for line in code.lines() {
        let ticks = line.trim_start().bytes().take_while(|b| *b == b'`').count();
        longest = longest.max(ticks);
    }
    let fence = "`".repeat(longest.max(2) + 1);
    let lang = lang
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '#' | '_'))
        .collect::<String>();
    let body = code.strip_suffix('\n').unwrap_or(code);
    format!("{fence}{lang}\n{body}\n{fence}")
}

/// Reduce a name from the notebook to a single safe path component.
fn sanitise_filename(name: &str, default_ext: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').trim_matches('-').to_string();
    let cleaned = if cleaned.is_empty() {
        format!("file.{default_ext}")
    } else {
        cleaned
    };
    if cleaned.contains('.') {
        cleaned
    } else {
        format!("{cleaned}.{default_ext}")
    }
}

/// Whether the asset route will serve this name (ADR-0011's allowlist).
///
/// Restated here rather than imported: `berrywiki-import` does not depend on
/// the serve crate, and a copy that can drift is better than a dependency
/// that inverts the layering. The drift is caught by the CLI test that
/// uploads each imported extension.
fn is_servable(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "pdf" | "txt" | "md" | "csv"
    )
}

fn asset_href(page_id: &str, filename: &str) -> String {
    format!("assets/{page_id}/{filename}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(src: &str) -> ImportModel {
        parse_ctd(src, "testhash")
    }

    #[test]
    fn a_notebook_becomes_a_tree() {
        let m = model(
            r#"<?xml version="1.0"?>
<cherrytree>
  <node name="Parent" unique_id="1" prog_lang="custom-colors">
    <rich_text>Top level text.</rich_text>
    <node name="Child" unique_id="2" prog_lang="custom-colors">
      <rich_text>Nested text.</rich_text>
    </node>
  </node>
</cherrytree>"#,
        );
        assert_eq!(m.nodes.len(), 2);
        assert_eq!(m.nodes[0].title, "Parent");
        assert_eq!(m.nodes[0].parent, None);
        assert_eq!(m.nodes[1].title, "Child");
        assert_eq!(m.nodes[1].parent, Some(0));
        assert_eq!(m.nodes[0].body, "Top level text.");
        assert_eq!(m.depth(), 2);
    }

    #[test]
    fn styles_become_markdown() {
        let m = model(
            r#"<cherrytree><node name="S" unique_id="1">
<rich_text>plain </rich_text>
<rich_text weight="heavy">bold</rich_text>
<rich_text> </rich_text>
<rich_text style="italic">italic</rich_text>
<rich_text> </rich_text>
<rich_text strikethrough="true">gone</rich_text>
<rich_text> </rich_text>
<rich_text family="monospace">code</rich_text>
</node></cherrytree>"#,
        );
        assert_eq!(m.nodes[0].body, "plain **bold** _italic_ ~~gone~~ `code`");
    }

    #[test]
    fn a_heading_run_becomes_a_heading() {
        let m = model(
            r#"<cherrytree><node name="H" unique_id="1">
<rich_text scale="h2">Section</rich_text><rich_text>
body</rich_text>
</node></cherrytree>"#,
        );
        assert_eq!(m.nodes[0].body, "## Section\n\nbody");
    }

    #[test]
    fn prose_that_looks_like_markup_survives() {
        let m = model(
            r#"<cherrytree><node name="P" unique_id="1">
<rich_text># not a heading and 2 * 3 * 4</rich_text>
</node></cherrytree>"#,
        );
        assert_eq!(m.nodes[0].body, "\\# not a heading and 2 \\* 3 \\* 4");
    }

    #[test]
    fn char_offset_counts_characters_not_bytes() {
        // The splice must count characters, and this fixture is shaped to
        // prove it. A one-run fixture cannot: the split is character-indexed
        // either way, so only the *running total* between runs differs, and
        // a single run has no running total. So there are two runs, with
        // non-ASCII in both.
        //
        // "h\u{e9}llo " is 6 characters and 7 bytes; "w\u{f6}rld" is 5 and 6.
        // A widget at character 8 belongs after "h\u{e9}llo w\u{f6}". A
        // byte-counted total puts it one character earlier.
        //
        // The source is written with escapes rather than literal accented
        // letters so the fixture cannot be silently mangled by a tool that
        // rewrites this file.
        let src = concat!(
            "<cherrytree><node name=\"O\" unique_id=\"1\">\n",
            "<rich_text>h\u{e9}llo </rich_text><rich_text>w\u{f6}rld</rich_text>\n",
            "<codebox char_offset=\"8\" syntax_highlighting=\"sh\">echo hi</codebox>\n",
            "</node></cherrytree>"
        );
        let m = model(src);
        assert_eq!(
            m.nodes[0].body, "h\u{e9}llo w\u{f6}\n\n```sh\necho hi\n```\n\nrld",
            "the widget landed at the wrong character"
        );
    }

    #[test]
    fn an_image_becomes_an_asset_and_a_link() {
        // An 8-byte PNG signature is enough: the importer never decodes the
        // image, it only moves the bytes.
        let m = model(
            r#"<cherrytree><node name="I" unique_id="1">
<rich_text>before</rich_text>
<encoded_png char_offset="6">iVBORw0KGgo=</encoded_png>
</node></cherrytree>"#,
        );
        assert_eq!(m.nodes[0].assets.len(), 1);
        assert_eq!(m.nodes[0].assets[0].filename, "image-1.png");
        assert_eq!(
            m.nodes[0].assets[0].bytes,
            vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
        let href = asset_href(&m.nodes[0].id, "image-1.png");
        assert!(m.nodes[0].body.contains(&format!("![]({href})")));
        assert_eq!(m.asset_count(), 1);
    }

    #[test]
    fn a_node_link_becomes_a_wiki_link_even_when_it_points_forwards() {
        let m = model(
            r#"<cherrytree>
<node name="First" unique_id="1">
<rich_text link="node 2">see the other one</rich_text>
</node>
<node name="Second" unique_id="2"><rich_text>hi</rich_text></node>
</cherrytree>"#,
        );
        assert_eq!(m.nodes[0].body, "[[Second]]");
    }

    #[test]
    fn a_web_link_becomes_a_markdown_link() {
        let m = model(
            r#"<cherrytree><node name="W" unique_id="1">
<rich_text link="webs https://example.com/a">here</rich_text>
</node></cherrytree>"#,
        );
        assert_eq!(m.nodes[0].body, "[here](https://example.com/a)");
    }

    #[test]
    fn a_file_link_keeps_the_text_and_says_what_it_dropped() {
        let m = model(
            r#"<cherrytree><node name="F" unique_id="1">
<rich_text link="file L2hvbWUvbWU=">my notes</rich_text>
</node></cherrytree>"#,
        );
        assert_eq!(m.nodes[0].body, "my notes");
        assert!(m
            .diagnostics
            .iter()
            .any(|d| d.code == "import.ct.link-to-file-dropped"));
    }

    #[test]
    fn a_code_node_is_one_fenced_block() {
        let m = model(
            r#"<cherrytree><node name="Script" unique_id="1" prog_lang="python">
<rich_text>x = 2 * 3
print(x)</rich_text>
</node></cherrytree>"#,
        );
        assert_eq!(m.nodes[0].body, "```python\nx = 2 * 3\nprint(x)\n```");
    }

    #[test]
    fn a_table_becomes_a_markdown_table_and_says_what_it_assumed() {
        let m = model(
            r#"<cherrytree><node name="T" unique_id="1">
<table char_offset="0">
<row><cell>Name</cell><cell>Value</cell></row>
<row><cell>a</cell><cell>1</cell></row>
</table>
</node></cherrytree>"#,
        );
        assert_eq!(
            m.nodes[0].body,
            "| Name | Value |\n| --- | --- |\n| a | 1 |"
        );
        assert!(m
            .diagnostics
            .iter()
            .any(|d| d.code == "import.ct.table-header-assumed"));
    }

    #[test]
    fn a_fence_inside_a_codebox_cannot_close_it() {
        let m = model(
            r#"<cherrytree><node name="C" unique_id="1">
<codebox char_offset="0" syntax_highlighting="markdown">```
inner
```</codebox>
</node></cherrytree>"#,
        );
        assert!(
            m.nodes[0].body.starts_with("````markdown"),
            "{:?}",
            m.nodes[0].body
        );
        assert!(m.nodes[0].body.ends_with("````"));
    }

    #[test]
    fn a_file_that_is_not_cherrytree_is_refused_not_half_read() {
        let m = model("<html><body><node name=\"x\"/></body></html>");
        assert!(m.nodes.is_empty());
        assert_eq!(m.diagnostics.len(), 1);
        assert_eq!(m.diagnostics[0].code, "import.ct.not-cherrytree");
        assert_eq!(m.diagnostics[0].severity, Severity::Error);
    }

    #[test]
    fn importing_the_same_notebook_twice_gives_the_same_ids() {
        let src = r#"<cherrytree><node name="A" unique_id="1"><rich_text>x</rich_text></node></cherrytree>"#;
        let a = parse_ctd(src, "hash");
        let b = parse_ctd(src, "hash");
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_notebook_gives_different_ids() {
        let src = r#"<cherrytree><node name="A" unique_id="1"><rich_text>x</rich_text></node></cherrytree>"#;
        let a = parse_ctd(src, "hash-one");
        let b = parse_ctd(src, "hash-two");
        assert_ne!(a.nodes[0].id, b.nodes[0].id);
    }

    #[test]
    fn sibling_title_collisions_are_reported_not_silently_merged() {
        let m = model(
            r#"<cherrytree>
<node name="Notes" unique_id="1"><rich_text>a</rich_text></node>
<node name="Notes" unique_id="2"><rich_text>b</rich_text></node>
</cherrytree>"#,
        );
        assert_eq!(m.nodes.len(), 2);
        assert_ne!(m.nodes[0].id, m.nodes[1].id);
        assert_eq!(m.title_collisions(), vec!["Notes".to_string()]);
    }

    #[test]
    fn malformed_xml_degrades_with_a_diagnostic_and_keeps_what_it_read() {
        let m = model(
            r#"<cherrytree><node name="Good" unique_id="1"><rich_text>kept</rich_text></node><node name="#,
        );
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].body, "kept");
        assert!(m.diagnostics.iter().any(|d| d.severity == Severity::Error));
    }

    #[test]
    fn an_empty_notebook_is_not_an_error() {
        let m = model("<cherrytree></cherrytree>");
        assert!(m.nodes.is_empty());
        assert!(m.diagnostics.is_empty());
    }
}
