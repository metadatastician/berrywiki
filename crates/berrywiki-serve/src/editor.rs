// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! P2-edit: source editor + preview, create/update/delete via the store, and
//! explicit Save / Save-draft with visible unsaved state (ADR-0006).
//!
//! The zero-JS rules shape everything here:
//! * One plain form per page; multiple actions via named submit buttons.
//! * Every successful POST answers 303 See Other (Post/Redirect/Get); notices
//!   ride a `notice` query parameter, never cookies.
//! * An error response NEVER discards submitted text: the form re-renders with
//!   the text intact and — for a refused Save — the text is also persisted as
//!   a draft, so even a closed tab loses nothing. That is the no-data-loss
//!   rule applied to an explicit action, not silent autosave.
//! * Two staleness layers guard a Save. The store's mtime+len fingerprint
//!   (ADR-0008) catches on-disk changes since the store's last reload; the
//!   hidden `base` hash catches changes since THIS editor was opened — the
//!   store re-fingerprints after every mutation, so its guard alone would let
//!   two sequential in-app editors silently clobber each other.

use std::collections::HashSet;

use berrywiki_core::PageKind;
use berrywiki_render::render_markdown;
use berrywiki_store::{CreatePageInput, MovePageInput, MovePlan, StoreError, WikiStore};
use berrywiki_sync::{SyncError, SyncOutcome};

use crate::{
    changes_page, escape_attr, escape_html, form_value, ids, layout, normalize_newlines,
    not_found_page, percent_decode, query_value, source_hash, App, Ctx, Recorded, Response,
};

// --- POST dispatch ---------------------------------------------------------

pub(crate) fn handle_post(app: &mut App, path: &str, body: &str) -> Response {
    if path == "/new" {
        return post_new(app, body);
    }
    if path == "/reload" {
        return post_reload(app, body);
    }
    if path == "/sync" {
        return post_sync(app);
    }
    if let Some(rest) = path.strip_prefix("/page/") {
        if let Some(id) = rest.strip_suffix("/edit") {
            return post_edit(app, &percent_decode(id), body);
        }
        if let Some(id) = rest.strip_suffix("/delete") {
            return post_delete(app, &percent_decode(id));
        }
        if let Some(id) = rest.strip_suffix("/move") {
            return post_move(app, &percent_decode(id), body);
        }
    }
    not_found_page(app.ctx(), path)
}

// --- the editor view -------------------------------------------------------

/// Everything one render of the editor needs. Collected in a struct because
/// the same view serves five states: fresh open, draft-resume, preview,
/// validation error and stale conflict.
struct EditorView<'a> {
    id: &'a str,
    title: &'a str,
    /// Textarea content (already LF-normalised).
    body: &'a str,
    /// The hidden base hash the form carries forward.
    base: &'a str,
    status: u16,
    notice: Option<String>,
    error: Option<String>,
    /// The submitted text was persisted as a draft after a refused Save.
    kept_as_draft: bool,
    /// A draft is being shown instead of the saved page.
    editing_draft: Option<DraftBanner>,
    /// Rendered preview HTML (from `berrywiki-render`, so already safe).
    preview: Option<String>,
}

struct DraftBanner {
    identical: bool,
}

pub(crate) fn edit_form(ctx: Ctx<'_>, id: &str, query: &str) -> Response {
    let Ok(page) = ctx.store.read_page(id) else {
        return not_found_page(ctx, id);
    };
    let draft = ctx.drafts.and_then(|d| d.load(id).ok().flatten());
    let (body, editing_draft) = match &draft {
        Some(d) => (
            d.content.clone(),
            Some(DraftBanner {
                identical: d.content == page.body,
            }),
        ),
        None => (page.body.clone(), None),
    };
    render_editor(
        ctx,
        &EditorView {
            id,
            title: &page.title,
            body: &body,
            base: &source_hash(&page.source),
            status: 200,
            notice: notice_text(&query_value(query, "notice")),
            error: None,
            kept_as_draft: false,
            editing_draft,
            preview: None,
        },
    )
}

fn notice_text(token: &str) -> Option<String> {
    // Fixed tokens only — the query string never reaches the page verbatim.
    match token {
        "draft-saved" => Some("Draft saved. The page itself is unchanged.".to_string()),
        "draft-discarded" => Some("Draft discarded.".to_string()),
        "reloaded" => Some("Wiki reloaded from disk.".to_string()),
        _ => None,
    }
}

fn render_editor(ctx: Ctx<'_>, v: &EditorView<'_>) -> Response {
    let id_a = escape_attr(v.id);
    let mut main = String::new();

    if let Some(n) = &v.notice {
        main.push_str(&format!("<p class=\"notice\">{}</p>", escape_html(n)));
    }
    if let Some(e) = &v.error {
        main.push_str(&format!("<p class=\"error-banner\">{}</p>", escape_html(e)));
    }
    if v.kept_as_draft {
        main.push_str(
            "<p class=\"draft-banner\">Your text has been kept as a draft — nothing is lost. \
             Reload the wiki below, then merge and save.</p>",
        );
        main.push_str(&format!(
            "<form class=\"inline-form\" method=\"post\" action=\"/reload\">\
             <input type=\"hidden\" name=\"back\" value=\"/page/{id_a}/edit\">\
             <button type=\"submit\">Reload the wiki from disk</button></form>"
        ));
    }
    if let Some(d) = &v.editing_draft {
        let note = if d.identical {
            " (identical to the saved page)"
        } else {
            ""
        };
        main.push_str(&format!(
            "<p class=\"draft-banner\">Unsaved draft{note} — you are editing the draft, \
             not the saved page.</p>"
        ));
    }

    main.push_str(&format!(
        "<form class=\"editor\" method=\"post\" action=\"/page/{id_a}/edit\">\
         <input type=\"hidden\" name=\"base\" value=\"{}\">\
         <div class=\"editor-field\"><label for=\"body\">Markdown source</label>\
         <textarea id=\"body\" name=\"body\" rows=\"24\">\n{}</textarea></div>\
         <div class=\"editor-buttons\">\
         <button type=\"submit\" name=\"action\" value=\"save\">Save</button>{}\
         <button type=\"submit\" name=\"action\" value=\"preview\" class=\"secondary\">Preview</button>{}\
         </div></form>",
        escape_attr(v.base),
        escape_html(v.body),
        if ctx.drafts.is_some() {
            "<button type=\"submit\" name=\"action\" value=\"save-draft\" class=\"secondary\">Save draft</button>"
        } else {
            ""
        },
        if v.editing_draft.is_some() {
            "<button type=\"submit\" name=\"action\" value=\"discard-draft\" class=\"secondary\">Discard draft</button>"
        } else {
            ""
        },
    ));

    if ctx.drafts.is_none() {
        main.push_str(
            "<p class=\"drafts-unavailable\">Save-draft is unavailable: the app-state \
             directory could not be resolved. Save still works.</p>",
        );
    }
    main.push_str(&format!(
        "<p class=\"page-actions\"><a href=\"/page/{id_a}\">View page</a> · \
         <a class=\"danger\" href=\"/page/{id_a}/delete\">Delete…</a></p>"
    ));
    if let Some(p) = &v.preview {
        main.push_str(&format!(
            "<section class=\"preview\"><h2>Preview</h2>\
             <article class=\"page\">{p}</article></section>"
        ));
    }

    let title = format!("Edit — {}", v.title);
    let body = layout(ctx, Some(v.id), &title, main);
    Response::html(v.status, body)
}

// --- edit POST -------------------------------------------------------------

fn post_edit(app: &mut App, id: &str, form: &str) -> Response {
    let submitted = normalize_newlines(&form_value(form, "body"));
    let action = form_value(form, "action");
    let base = form_value(form, "base");

    // Snapshot what rendering needs before any mutable borrow of the store.
    let (title, current_hash) = match app.store().read_page(id) {
        Ok(p) => (p.title.clone(), source_hash(&p.source)),
        Err(_) => return not_found_page(app.ctx(), id),
    };

    match action.as_str() {
        "preview" => render_editor(
            app.ctx(),
            &EditorView {
                id,
                title: &title,
                body: &submitted,
                // Carry the ORIGINAL base forward: previewing must not widen
                // the window for a silent clobber.
                base: &base,
                status: 200,
                notice: None,
                error: None,
                kept_as_draft: false,
                editing_draft: None,
                preview: Some(render_markdown(&submitted)),
            },
        ),
        "save-draft" => match &app.drafts {
            Some(drafts) => match drafts.save(id, &submitted) {
                Ok(()) => Response::see_other(format!("/page/{id}/edit?notice=draft-saved")),
                Err(e) => editor_error(app.ctx(), id, &title, &submitted, &base, e.to_string()),
            },
            None => editor_error(
                app.ctx(),
                id,
                &title,
                &submitted,
                &base,
                "Drafts are unavailable in this session.".to_string(),
            ),
        },
        "discard-draft" => {
            if let Some(drafts) = &app.drafts {
                // Discard is idempotent; an I/O failure degrades to a banner.
                if let Err(e) = drafts.discard(id) {
                    return editor_error(app.ctx(), id, &title, &submitted, &base, e.to_string());
                }
            }
            Response::see_other(format!("/page/{id}/edit?notice=draft-discarded"))
        }
        "save" => {
            if base != current_hash {
                return stale_conflict(
                    app,
                    id,
                    &title,
                    &submitted,
                    &current_hash,
                    "The page changed after this editor was opened.".to_string(),
                );
            }
            match app.update_page(id, &submitted) {
                Ok(rec) => {
                    if let Some(drafts) = &app.drafts {
                        // The draft is superseded by the save; failure to
                        // remove it is only cosmetic.
                        let _ = drafts.discard(id);
                    }
                    Response::see_other(with_notice(&format!("/page/{id}"), rec))
                }
                // A refusal that keeps the text: stale write, or a clone that
                // must not be committed to (merge in progress, detached HEAD).
                Err(e) if is_refusal(&e) => {
                    stale_conflict(app, id, &title, &submitted, &current_hash, e.to_string())
                }
                Err(e) => editor_error(app.ctx(), id, &title, &submitted, &base, e.to_string()),
            }
        }
        _ => editor_error(
            app.ctx(),
            id,
            &title,
            &submitted,
            &base,
            "Unknown editor action.".to_string(),
        ),
    }
}

/// Append the post-save notice token (commit-on-save only) to a redirect.
fn with_notice(target: &str, rec: Recorded) -> String {
    match rec.notice_token() {
        Some(t) => format!("{target}?notice={t}"),
        None => target.to_string(),
    }
}

/// Errors where the right answer is "keep the text, let the user sort out the
/// repository": the store's stale guard, and sync's refusals to commit into a
/// clone mid-merge or with a detached HEAD. Everything else is a plain error.
fn is_refusal(e: &SyncError) -> bool {
    matches!(
        e,
        SyncError::Store(StoreError::StaleWrite { .. })
            | SyncError::UnmergedPaths { .. }
            | SyncError::DetachedHead
    )
}

/// A refused Save: 409, submitted text intact in the form AND persisted as a
/// draft, plus the reload affordance.
fn stale_conflict(
    app: &App,
    id: &str,
    title: &str,
    submitted: &str,
    base: &str,
    detail: String,
) -> Response {
    let kept = app
        .drafts
        .as_ref()
        .map(|d| d.save(id, submitted).is_ok())
        .unwrap_or(false);
    render_editor(
        app.ctx(),
        &EditorView {
            id,
            title,
            body: submitted,
            base,
            status: 409,
            notice: None,
            error: Some(format!("Save refused: {detail}")),
            kept_as_draft: kept,
            editing_draft: None,
            preview: None,
        },
    )
}

/// A non-stale editor error: 400, submitted text intact, nothing written.
fn editor_error(
    ctx: Ctx<'_>,
    id: &str,
    title: &str,
    submitted: &str,
    base: &str,
    detail: String,
) -> Response {
    render_editor(
        ctx,
        &EditorView {
            id,
            title,
            body: submitted,
            base,
            status: 400,
            notice: None,
            error: Some(detail),
            kept_as_draft: false,
            editing_draft: None,
            preview: None,
        },
    )
}

// --- create ----------------------------------------------------------------

struct NewView<'a> {
    title_value: &'a str,
    parent_value: &'a str,
    body: &'a str,
    status: u16,
    error: Option<String>,
    preview: Option<String>,
}

pub(crate) fn new_form(ctx: Ctx<'_>, query: &str) -> Response {
    render_new(
        ctx,
        &NewView {
            title_value: "",
            parent_value: &query_value(query, "parent"),
            body: "",
            status: 200,
            error: None,
            preview: None,
        },
    )
}

fn render_new(ctx: Ctx<'_>, v: &NewView<'_>) -> Response {
    let mut main = String::new();
    if let Some(e) = &v.error {
        main.push_str(&format!("<p class=\"error-banner\">{}</p>", escape_html(e)));
    }

    // Parent selector: managed pages only — an unmanaged page has no stable id
    // and cannot own children (the store would refuse it anyway).
    let mut options = String::from("<option value=\"\">(top level)</option>");
    for (depth, page) in ctx.store.graph().walk() {
        if page.metadata.is_none() {
            continue;
        }
        let selected = if page.id == v.parent_value {
            " selected"
        } else {
            ""
        };
        options.push_str(&format!(
            "<option value=\"{}\"{selected}>{}{}</option>",
            escape_attr(&page.id),
            "\u{a0}\u{a0}".repeat(depth),
            escape_html(&page.title),
        ));
    }

    main.push_str(&format!(
        "<form class=\"editor\" method=\"post\" action=\"/new\">\
         <div class=\"editor-field\"><label for=\"title\">Title</label>\
         <input id=\"title\" name=\"title\" value=\"{}\" required></div>\
         <div class=\"editor-field\"><label for=\"parent\">Parent</label>\
         <select id=\"parent\" name=\"parent\">{options}</select></div>\
         <div class=\"editor-field\"><label for=\"body\">Markdown source (optional; \
         a title heading is added when absent)</label>\
         <textarea id=\"body\" name=\"body\" rows=\"16\">\n{}</textarea></div>\
         <div class=\"editor-buttons\">\
         <button type=\"submit\" name=\"action\" value=\"create\">Create</button>\
         <button type=\"submit\" name=\"action\" value=\"preview\" class=\"secondary\">Preview</button>\
         </div></form>",
        escape_attr(v.title_value),
        escape_html(v.body),
    ));

    if let Some(p) = &v.preview {
        main.push_str(&format!(
            "<section class=\"preview\"><h2>Preview</h2>\
             <article class=\"page\">{p}</article></section>"
        ));
    }

    let body = layout(ctx, None, "New page", main);
    Response::html(v.status, body)
}

fn post_new(app: &mut App, form: &str) -> Response {
    let title = form_value(form, "title").trim().to_string();
    let parent = form_value(form, "parent");
    let body_text = normalize_newlines(&form_value(form, "body"));
    let action = form_value(form, "action");

    if action == "preview" {
        return render_new(
            app.ctx(),
            &NewView {
                title_value: &title,
                parent_value: &parent,
                body: &body_text,
                status: 200,
                error: None,
                preview: Some(render_markdown(&body_text)),
            },
        );
    }

    if title.is_empty() {
        return render_new(
            app.ctx(),
            &NewView {
                title_value: &title,
                parent_value: &parent,
                body: &body_text,
                status: 400,
                error: Some("A page needs a title.".to_string()),
                preview: None,
            },
        );
    }

    let parent_id = if parent.is_empty() {
        None
    } else {
        Some(parent.clone())
    };
    let position = next_position(app, parent_id.as_deref());
    let input = CreatePageInput {
        id: ids::new_page_id(),
        title: title.clone(),
        parent_id,
        position,
        kind: PageKind::Page,
        tags: Vec::new(),
        body: body_text.clone(),
    };
    match app.create_page(input) {
        Ok((id, rec)) => Response::see_other(with_notice(&format!("/page/{id}"), rec)),
        Err(e) => render_new(
            app.ctx(),
            &NewView {
                title_value: &title,
                parent_value: &parent,
                body: &body_text,
                status: 400,
                error: Some(e.to_string()),
                preview: None,
            },
        ),
    }
}

/// New pages append after their siblings: max sibling position + 1.
fn next_position(app: &App, parent_id: Option<&str>) -> i64 {
    let graph = app.store().graph();
    let siblings = match parent_id {
        Some(p) => graph.children_of(p),
        None => graph.roots(),
    };
    siblings
        .iter()
        .map(|s| s.position())
        .max()
        .map(|m| m.saturating_add(1))
        .unwrap_or(0)
}

// --- delete ----------------------------------------------------------------

pub(crate) fn delete_confirm(ctx: Ctx<'_>, id: &str, error: Option<(u16, String)>) -> Response {
    let Ok(page) = ctx.store.read_page(id) else {
        return not_found_page(ctx, id);
    };
    let child_count = ctx.store.graph().children_of(id).len();
    let id_a = escape_attr(id);
    let mut main = String::new();
    let status = match &error {
        Some((s, e)) => {
            main.push_str(&format!("<p class=\"error-banner\">{}</p>", escape_html(e)));
            *s
        }
        None => 200,
    };

    if child_count > 0 {
        main.push_str(&format!(
            "<p>“{}” still has {child_count} child page(s). The store refuses to \
             delete a page with children — move or delete them first.</p>\
             <p class=\"page-actions\"><a href=\"/page/{id_a}\">Back to the page</a></p>",
            escape_html(&page.title),
        ));
    } else {
        main.push_str(&format!(
            "<p>Delete “{}” permanently? The page file is removed from the wiki \
             folder and the sidebar is regenerated in the same operation. \
             (Anything already committed to git stays in history.)</p>\
             <form method=\"post\" action=\"/page/{id_a}/delete\">\
             <div class=\"editor-buttons\">\
             <button type=\"submit\">Delete permanently</button></div></form>\
             <p class=\"page-actions\"><a href=\"/page/{id_a}\">Cancel</a></p>",
            escape_html(&page.title),
        ));
    }

    let title = format!("Delete — {}", page.title);
    let body = layout(ctx, Some(id), &title, main);
    Response::html(status, body)
}

fn post_delete(app: &mut App, id: &str) -> Response {
    // The redirect target must be captured before the page is gone.
    let parent = match app.store().read_page(id) {
        Ok(p) => p.metadata.as_ref().and_then(|m| m.parent_id.clone()),
        Err(_) => return not_found_page(app.ctx(), id),
    };
    match app.delete_page(id) {
        Ok(rec) => {
            if let Some(drafts) = &app.drafts {
                let _ = drafts.discard(id);
            }
            match parent {
                Some(p) => Response::see_other(with_notice(&format!("/page/{p}"), rec)),
                None => Response::see_other(with_notice("/", rec)),
            }
        }
        Err(e) if is_refusal(&e) => delete_confirm(app.ctx(), id, Some((409, e.to_string()))),
        Err(e) => delete_confirm(app.ctx(), id, Some((400, e.to_string()))),
    }
}

// --- move ------------------------------------------------------------------
//
// P2-move UI over the store's transactional subtree move. **Preview is a dry
// run** (`LocalFolderStore::plan_move`), which writes nothing; **Move
// re-plans against the live graph** and applies. A previewed plan is never
// carried in the form, so a preview can never be applied to a wiki that
// changed after it was shown — the user only ever confirms "move this page
// here", and the store decides afresh what that means.
//
// The hidden `from_parent` / `from_position` pair is the move form's `base`,
// for the same reason the editor carries one: the store's fingerprint guard
// sees only on-disk changes since its last reload, so without it a confirm
// after another editor moved the same page would re-parent from a starting
// point this user never saw. A mismatch is a 409 with the form refreshed.
//
// Retitling is not part of a move: the title is the page's H1, edited in the
// editor; the filename follows the title on the next move (the planner
// recomputes every name from current titles).

struct MoveView<'a> {
    id: &'a str,
    title: &'a str,
    /// The `<select>` value: a parent id, or `""` for top level.
    parent_value: &'a str,
    position_value: &'a str,
    /// Where the page is right now, carried as the form's base.
    from_parent: &'a str,
    from_position: &'a str,
    status: u16,
    error: Option<String>,
    /// A dry run to show under the form.
    plan: Option<&'a MovePlan>,
}

pub(crate) fn move_form(ctx: Ctx<'_>, id: &str) -> Response {
    let Ok(page) = ctx.store.read_page(id) else {
        return not_found_page(ctx, id);
    };
    if page.metadata.is_none() {
        return unmanaged_page(ctx, id, &page.title);
    }
    let parent = page.parent_id().unwrap_or("").to_string();
    let position = page.position().to_string();
    render_move(
        ctx,
        &MoveView {
            id,
            title: &page.title,
            parent_value: &parent,
            position_value: &position,
            from_parent: &parent,
            from_position: &position,
            status: 200,
            error: None,
            plan: None,
        },
    )
}

/// A page without a metadata block has no place in the tree, so there is
/// nothing to move it from; the store would refuse (`UnmanagedPage`).
fn unmanaged_page(ctx: Ctx<'_>, id: &str, title: &str) -> Response {
    Response::html(
        400,
        layout(
            ctx,
            Some(id),
            &format!("Move — {title}"),
            format!(
                "<p class=\"error-banner\">“{}” has no BerryWiki metadata block, so it \
                 is outside the tree and cannot be moved within it.</p>\
                 <p class=\"page-actions\"><a href=\"/page/{}\">Back to the page</a></p>",
                escape_html(title),
                escape_attr(id),
            ),
        ),
    )
}

fn render_move(ctx: Ctx<'_>, v: &MoveView<'_>) -> Response {
    let id_a = escape_attr(v.id);
    let mut main = String::new();
    if let Some(e) = &v.error {
        main.push_str(&format!("<p class=\"error-banner\">{}</p>", escape_html(e)));
    }

    // The page's own subtree is never offered as a destination: that is the
    // cycle the store refuses, so the form does not present it as a choice.
    // (The store still enforces it on a forged POST.)
    let graph = ctx.store.graph();
    let mut excluded: HashSet<String> = HashSet::new();
    let mut queue = vec![v.id.to_string()];
    while let Some(next) = queue.pop() {
        for child in graph.children_of(&next) {
            queue.push(child.id.clone());
        }
        excluded.insert(next);
    }
    let mut options = String::from("<option value=\"\">(top level)</option>");
    for (depth, page) in graph.walk() {
        if page.metadata.is_none() || excluded.contains(&page.id) {
            continue;
        }
        let selected = if page.id == v.parent_value {
            " selected"
        } else {
            ""
        };
        options.push_str(&format!(
            "<option value=\"{}\"{selected}>{}{}</option>",
            escape_attr(&page.id),
            "\u{a0}\u{a0}".repeat(depth),
            escape_html(&page.title),
        ));
    }

    main.push_str(&format!(
        "<p>Move “{}” to another parent or position. Its subpages come with it. \
         Because filenames follow the ancestry, the files of the whole subtree are \
         renamed and every link to them across the wiki is rewritten, in one \
         operation{}. <strong>Preview</strong> lists what would change without \
         changing anything. (To rename the page, edit its title heading.)</p>\
         <form class=\"editor\" method=\"post\" action=\"/page/{id_a}/move\">\
         <input type=\"hidden\" name=\"from_parent\" value=\"{}\">\
         <input type=\"hidden\" name=\"from_position\" value=\"{}\">\
         <div class=\"editor-field\"><label for=\"parent\">Parent</label>\
         <select id=\"parent\" name=\"parent\">{options}</select></div>\
         <div class=\"editor-field\"><label for=\"position\">Position among siblings \
         (lower sorts first)</label>\
         <input id=\"position\" name=\"position\" type=\"number\" value=\"{}\" required></div>\
         <div class=\"editor-buttons\">\
         <button type=\"submit\" name=\"action\" value=\"preview\" class=\"secondary\">Preview</button>\
         <button type=\"submit\" name=\"action\" value=\"move\">Move</button>\
         </div></form>",
        escape_html(v.title),
        if ctx.sync.is_some() {
            " and one commit"
        } else {
            ""
        },
        escape_attr(v.from_parent),
        escape_attr(v.from_position),
        escape_attr(v.position_value),
    ));

    if let Some(plan) = v.plan {
        main.push_str(&render_plan(ctx, plan));
    }
    main.push_str(&format!(
        "<p class=\"page-actions\"><a href=\"/page/{id_a}\">Cancel</a></p>"
    ));

    let title = format!("Move — {}", v.title);
    let body = layout(ctx, Some(v.id), &title, main);
    Response::html(v.status, body)
}

/// The dry run, rendered. Every row comes from the same staged plan the
/// store would apply, so the list is exact, not an estimate.
fn render_plan(ctx: Ctx<'_>, plan: &MovePlan) -> String {
    let destination = match &plan.new_parent_id {
        Some(p) => {
            let title = ctx
                .store
                .read_page(p)
                .map(|q| q.title.clone())
                .unwrap_or_else(|_| p.clone());
            format!("a child of “{}”", escape_html(&title))
        }
        None => "a top-level page".to_string(),
    };
    let mut s = format!(
        "<section class=\"preview\"><h2>What this move would do</h2>\
         <p>The page becomes {destination}, at position {}.</p>",
        plan.new_position
    );
    if plan.renames.is_empty() {
        s.push_str("<p>No files are renamed.</p>");
    } else {
        s.push_str(
            "<table class=\"plan\"><tr><th>Page</th><th>File now</th><th>File after</th></tr>",
        );
        for r in &plan.renames {
            s.push_str(&format!(
                "<tr><td>{}</td><td><code>{}</code></td><td><code>{}</code></td></tr>",
                escape_html(&r.title),
                escape_html(&r.old_path),
                escape_html(&r.new_path),
            ));
        }
        s.push_str("</table>");
    }
    if plan.link_rewrites.is_empty() {
        s.push_str("<p>No links elsewhere need rewriting.</p>");
    } else {
        s.push_str("<p>Links rewritten in place:</p><ul class=\"plan-rewrites\">");
        for w in &plan.link_rewrites {
            s.push_str(&format!(
                "<li>{} (<code>{}</code>)</li>",
                escape_html(&w.title),
                escape_html(&w.path),
            ));
        }
        s.push_str("</ul>");
    }
    s.push_str("<p>Nothing has been changed. Press <strong>Move</strong> to apply.</p></section>");
    s
}

fn post_move(app: &mut App, id: &str, form: &str) -> Response {
    let (title, cur_parent, cur_position, managed) = match app.store().read_page(id) {
        Ok(p) => (
            p.title.clone(),
            p.parent_id().unwrap_or("").to_string(),
            p.position().to_string(),
            p.metadata.is_some(),
        ),
        Err(_) => return not_found_page(app.ctx(), id),
    };
    if !managed {
        return unmanaged_page(app.ctx(), id, &title);
    }
    let parent = form_value(form, "parent");
    let position = form_value(form, "position");
    let action = form_value(form, "action");

    // Re-render with the submitted values kept and the base refreshed to
    // where the page is now.
    let mut v = MoveView {
        id,
        title: &title,
        parent_value: &parent,
        position_value: &position,
        from_parent: &cur_parent,
        from_position: &cur_position,
        status: 200,
        error: None,
        plan: None,
    };

    if form_value(form, "from_parent") != cur_parent
        || form_value(form, "from_position") != cur_position
    {
        v.status = 409;
        v.error = Some(
            "This page was moved after this form was opened, so the move was not made. \
             The form now shows where the page is; check the destination and submit again."
                .to_string(),
        );
        return render_move(app.ctx(), &v);
    }

    let new_position: i64 = match position.trim().parse() {
        Ok(n) => n,
        Err(_) => {
            v.status = 400;
            v.error = Some("Position must be a whole number.".to_string());
            return render_move(app.ctx(), &v);
        }
    };
    let input = MovePageInput {
        id: id.to_string(),
        new_parent_id: if parent.is_empty() {
            None
        } else {
            Some(parent.clone())
        },
        new_position,
    };

    if action == "preview" {
        return match app.store().plan_move(&input) {
            Ok(plan) => {
                v.plan = Some(&plan);
                render_move(app.ctx(), &v)
            }
            Err(e) => {
                let e = SyncError::from(e);
                v.status = if is_refusal(&e) { 409 } else { 400 };
                v.error = Some(e.to_string());
                render_move(app.ctx(), &v)
            }
        };
    }

    match app.move_page(input) {
        Ok(rec) => Response::see_other(with_notice(&format!("/page/{id}"), rec)),
        Err(e) => {
            v.status = if is_refusal(&e) { 409 } else { 400 };
            v.error = Some(e.to_string());
            render_move(app.ctx(), &v)
        }
    }
}

// --- sync ------------------------------------------------------------------

/// `POST /sync`: fetch, fast-forward, push — bounded retries, never a merge,
/// never a force. Diverged histories go to `/conflicts`; everything else
/// redirects to `/changes` with a fixed notice token.
fn post_sync(app: &mut App) -> Response {
    let Some(sync) = app.sync_mut() else {
        return Response::html(
            400,
            layout(
                app.ctx(),
                None,
                "Sync unavailable",
                "<p class=\"error-banner\">Commit-on-save is off in this session, so there \
                 is nothing for BerryWiki to synchronise. Use git directly.</p>"
                    .to_string(),
            ),
        );
    };
    match sync.sync_retrying(3) {
        Ok(report) => {
            let token = match &report.outcome {
                SyncOutcome::NoRemote => "synced-no-remote",
                SyncOutcome::UpToDate => "synced-up-to-date",
                SyncOutcome::Integrated { .. } => "synced-integrated",
                SyncOutcome::Published { .. } => "synced-published",
                SyncOutcome::PushRaced => "synced-raced",
                SyncOutcome::Diverged(_) => {
                    app.last_sync = Some(report.outcome);
                    return Response::see_other("/conflicts");
                }
            };
            app.last_sync = Some(report.outcome);
            Response::see_other(format!("/changes?notice={token}"))
        }
        Err(e) => {
            let msg = e.to_string();
            let mut r = changes_page(app.ctx(), "", Some(&msg));
            r.status = 500;
            r
        }
    }
}

// --- reload ----------------------------------------------------------------

fn post_reload(app: &mut App, form: &str) -> Response {
    let back = form_value(form, "back");
    // Local absolute paths only — no header injection, no open redirect.
    let target = if back.starts_with('/') && !back.starts_with("//") && !back.contains(['\r', '\n'])
    {
        back
    } else {
        "/".to_string()
    };
    match app.reload() {
        Ok(()) => {
            let sep = if target.contains('?') { '&' } else { '?' };
            Response::see_other(format!("{target}{sep}notice=reloaded"))
        }
        Err(e) => Response::html(
            500,
            layout(
                app.ctx(),
                None,
                "Reload failed",
                format!(
                    "<p class=\"error-banner\">Reload failed: {}</p>",
                    escape_html(&e.to_string())
                ),
            ),
        ),
    }
}
