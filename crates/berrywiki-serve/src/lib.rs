// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

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
use berrywiki_store::{CreatePageInput, LocalFolderStore, MovePageInput, WikiStore};
use berrywiki_sync::{ConflictReport, DivergedHandoff, Saved, SyncError, SyncOutcome, SyncedStore};

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
    pub(crate) backend: Backend,
    pub(crate) drafts: Option<DraftStore>,
    /// Outcome of the last `POST /sync` in this process, shown on `/changes`.
    pub(crate) last_sync: Option<SyncOutcome>,
}

/// Where editor mutations go. `Plain` writes files and stops (the caller
/// commits with git); `Synced` turns every mutation into one logical commit
/// through `berrywiki-sync`, sidebar in the same commit (ADR-0010).
pub(crate) enum Backend {
    Plain(LocalFolderStore),
    Synced(SyncedStore<LocalFolderStore>),
}

/// What one editor mutation recorded in git, for the post-save notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Recorded {
    /// The backend commits at all (`false` for `Backend::Plain`).
    pub(crate) synced: bool,
    /// Changes made outside BerryWiki were checkpointed as their own commit
    /// before this mutation.
    pub(crate) checkpoint: bool,
    /// The mutation itself produced a commit (`false` when the write was
    /// byte-identical, so there was nothing to commit).
    pub(crate) commit: bool,
}

impl Recorded {
    fn plain() -> Self {
        Recorded {
            synced: false,
            checkpoint: false,
            commit: false,
        }
    }
    fn from_saved<T>(s: &Saved<T>) -> Self {
        Recorded {
            synced: true,
            checkpoint: s.checkpoint.is_some(),
            commit: s.commit.is_some(),
        }
    }
    /// The fixed `notice` token for the redirect after a successful save;
    /// `None` in plain mode, where the page view has nothing to report.
    pub(crate) fn notice_token(self) -> Option<&'static str> {
        if !self.synced {
            None
        } else if self.commit && self.checkpoint {
            Some("committed-after-checkpoint")
        } else if self.commit {
            Some("committed")
        } else {
            Some("unchanged")
        }
    }
}

impl App {
    /// An editor over a plain folder (saves change files only), with the
    /// draft store wired through the store's app-state home
    /// (`$XDG_STATE_HOME/berrywiki/<repo-id>/drafts/`).
    pub fn new(store: LocalFolderStore) -> Self {
        let drafts = store.appstate().map(|a| DraftStore::new(a.drafts_dir()));
        App::with_drafts(store, drafts)
    }

    /// Explicit draft store (or `None` for the degraded mode) — used by tests
    /// so they never depend on environment variables.
    pub fn with_drafts(store: LocalFolderStore, drafts: Option<DraftStore>) -> Self {
        App {
            backend: Backend::Plain(store),
            drafts,
            last_sync: None,
        }
    }

    /// An editor with commit-on-save: every mutation is one logical commit.
    pub fn synced(store: SyncedStore<LocalFolderStore>) -> Self {
        let drafts = store
            .store()
            .appstate()
            .map(|a| DraftStore::new(a.drafts_dir()));
        App::synced_with_drafts(store, drafts)
    }

    /// Commit-on-save with an explicit draft store (tests).
    pub fn synced_with_drafts(
        store: SyncedStore<LocalFolderStore>,
        drafts: Option<DraftStore>,
    ) -> Self {
        App {
            backend: Backend::Synced(store),
            drafts,
            last_sync: None,
        }
    }

    pub fn store(&self) -> &LocalFolderStore {
        match &self.backend {
            Backend::Plain(s) => s,
            Backend::Synced(s) => s.store(),
        }
    }

    /// Whether editor mutations are committed (commit-on-save).
    pub fn commits_on_save(&self) -> bool {
        matches!(self.backend, Backend::Synced(_))
    }

    pub(crate) fn sync(&self) -> Option<&SyncedStore<LocalFolderStore>> {
        match &self.backend {
            Backend::Plain(_) => None,
            Backend::Synced(s) => Some(s),
        }
    }

    pub(crate) fn sync_mut(&mut self) -> Option<&mut SyncedStore<LocalFolderStore>> {
        match &mut self.backend {
            Backend::Plain(_) => None,
            Backend::Synced(s) => Some(s),
        }
    }

    pub(crate) fn ctx(&self) -> Ctx<'_> {
        Ctx {
            store: self.store(),
            drafts: self.drafts.as_ref(),
            editing: true,
            sync: self.sync(),
            last_sync: self.last_sync.as_ref(),
        }
    }

    // ----- mutations: the editor's only write path -----
    // Each goes through the backend so that, with commit-on-save, the store
    // write and its commit are one operation the editor cannot split.

    pub(crate) fn create_page(
        &mut self,
        input: CreatePageInput,
    ) -> Result<(String, Recorded), SyncError> {
        match &mut self.backend {
            Backend::Plain(s) => Ok((s.create_page(input)?, Recorded::plain())),
            Backend::Synced(s) => {
                let saved = s.create_page(input)?;
                let rec = Recorded::from_saved(&saved);
                Ok((saved.value, rec))
            }
        }
    }

    /// Save a body and a tag list together.
    ///
    /// Separate from [`WikiStore::update_page`] rather than a superset of it:
    /// a plain body save must never touch a hand-authored tag list, and on the
    /// synced backend this is one store call and therefore one commit. The
    /// editor is the only caller, so `App` carries no body-only counterpart.
    pub(crate) fn update_page_with_tags(
        &mut self,
        id: &str,
        body: &str,
        tags: &[String],
    ) -> Result<Recorded, SyncError> {
        match &mut self.backend {
            Backend::Plain(s) => {
                s.update_page_with_tags(id, body, tags)?;
                Ok(Recorded::plain())
            }
            Backend::Synced(s) => Ok(Recorded::from_saved(
                &s.update_page_with_tags(id, body, tags)?,
            )),
        }
    }

    pub(crate) fn delete_page(&mut self, id: &str) -> Result<Recorded, SyncError> {
        match &mut self.backend {
            Backend::Plain(s) => {
                s.delete_page(id)?;
                Ok(Recorded::plain())
            }
            Backend::Synced(s) => Ok(Recorded::from_saved(&s.delete_page(id)?)),
        }
    }

    /// Re-parent/reposition a page: the store's transactional subtree move
    /// (descendant filenames, inbound links, sidebar) as one operation and,
    /// with commit-on-save, one commit.
    pub(crate) fn move_page(&mut self, input: MovePageInput) -> Result<Recorded, SyncError> {
        match &mut self.backend {
            Backend::Plain(s) => {
                s.move_page(input)?;
                Ok(Recorded::plain())
            }
            Backend::Synced(s) => Ok(Recorded::from_saved(&s.move_page(input)?)),
        }
    }

    pub(crate) fn reload(&mut self) -> Result<(), SyncError> {
        match &mut self.backend {
            Backend::Plain(s) => Ok(s.reload()?),
            Backend::Synced(s) => s.reload(),
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
    /// The sync engine when commit-on-save is on; `None` in plain mode and on
    /// the read-only path.
    pub(crate) sync: Option<&'a SyncedStore<LocalFolderStore>>,
    pub(crate) last_sync: Option<&'a SyncOutcome>,
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
        if let Some(id) = rest.strip_suffix("/move") {
            return editor::move_form(ctx, &percent_decode(id));
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
            sync: None,
            last_sync: None,
        },
        path,
        query,
    )
}

fn dispatch(ctx: Ctx<'_>, path: &str, query: &str) -> Response {
    if path == "/" {
        return home_page(ctx, &query_value(query, "notice"));
    }
    if path == "/diagnostics" {
        return diagnostics_page(ctx);
    }
    if path == "/search" {
        return search_page(ctx, &query_value(query, "q"), &query_value(query, "tag"));
    }
    if path == "/changes" {
        return changes_page(ctx, &query_value(query, "notice"), None);
    }
    if path == "/conflicts" {
        return conflicts_page(ctx);
    }
    if path == "/tags" {
        return tags_index_page(ctx);
    }
    if let Some(rest) = path.strip_prefix("/tags/") {
        return tag_page(ctx, &percent_decode(rest));
    }
    if let Some(rest) = path.strip_prefix("/page/") {
        let notice = query_value(query, "notice");
        return page_view(ctx, &percent_decode(rest), &notice);
    }
    Response::html(
        404,
        layout(ctx, None, "Not found", "<p>No such page.</p>".to_string()),
    )
}

fn home_page(ctx: Ctx<'_>, notice: &str) -> Response {
    // Land on the first root page if there is one, else an empty-state.
    if let Some(root) = ctx.store.graph().roots().first() {
        return page_view(ctx, &root.id, notice);
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

/// Fixed notice tokens for the page view; the query string never reaches the
/// page verbatim (editor doctrine).
fn page_notice_text(token: &str) -> Option<&'static str> {
    match token {
        "committed" => Some("Saved and committed."),
        "committed-after-checkpoint" => Some(
            "Saved and committed. Changes made outside BerryWiki were first \
             recorded in their own commit.",
        ),
        "unchanged" => Some("Nothing changed, so nothing was committed."),
        "reloaded" => Some("Wiki reloaded from disk."),
        _ => None,
    }
}

fn page_view(ctx: Ctx<'_>, id: &str, notice: &str) -> Response {
    let page = match ctx.store.read_page(id) {
        Ok(p) => p,
        Err(_) => return not_found_page(ctx, id),
    };

    let mut main = String::new();
    if ctx.editing {
        if let Some(n) = page_notice_text(notice) {
            main.push_str(&format!("<p class=\"notice\">{n}</p>"));
        }
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
             <a href=\"/page/{id_a}/move\">Move…</a> · \
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

// --- git-facing pages (P3-serve-sync) ---------------------------------------
// Everything git says (branch names, subjects, stderr) passes through
// `redact_secret` and then `escape_html` before it reaches a response: the
// engine never puts the token on a command line, but a remote URL or a
// pasted page can carry one, and ADR-0002's rule is "never in logs or HTML".

/// Mask the outbound token wherever it appears in text destined for HTML.
pub(crate) fn redact_secret(text: &str) -> String {
    match std::env::var("BERRYWIKI_GITHUB_TOKEN") {
        Ok(tok) if !tok.is_empty() => text.replace(&tok, "[redacted]"),
        _ => text.to_string(),
    }
}

pub(crate) fn git_text(s: &str) -> String {
    escape_html(&redact_secret(s))
}

/// What one line of `/changes` says about the last `POST /sync`.
fn sync_outcome_text(outcome: &SyncOutcome) -> String {
    match outcome {
        SyncOutcome::NoRemote => {
            "No remote is configured, so commits stay local. Nothing to publish.".to_string()
        }
        SyncOutcome::UpToDate => "Local and remote already agree.".to_string(),
        SyncOutcome::Integrated { fetched } => {
            format!("Fast-forwarded onto {fetched} commit(s) from the remote.")
        }
        SyncOutcome::Published { pushed } => format!("Published {pushed} local commit(s)."),
        SyncOutcome::Diverged(h) => format!(
            "Local and remote have both moved ({} ahead, {} behind). Nothing was merged or \
             pushed; see the conflicts page.",
            h.ahead, h.behind
        ),
        SyncOutcome::PushRaced => "The remote advanced between fetch and push; nothing was \
                                   forced. Sync again to reclassify."
            .to_string(),
    }
}

fn sync_notice_text(token: &str) -> Option<&'static str> {
    match token {
        "synced-up-to-date" => Some("Synchronised: local and remote already agree."),
        "synced-published" => Some("Synchronised: local commits published."),
        "synced-integrated" => Some("Synchronised: remote commits integrated (fast-forward)."),
        "synced-no-remote" => Some("No remote is configured; commits stay local."),
        "merge-finished" => Some(
            "The merge was concluded: the navigation file was regenerated and one merge commit \
             recorded.",
        ),
        "synced-raced" => {
            Some("The remote moved while publishing; nothing was forced. Sync again to reclassify.")
        }
        _ => None,
    }
}

/// The one-line git status under the top bar. Rendered only in editing mode:
/// the read-only path has no repository to report on.
fn status_strip(ctx: Ctx<'_>) -> String {
    if !ctx.editing {
        return String::new();
    }
    let Some(sync) = ctx.sync else {
        return "<p class=\"status-strip\"><span class=\"status-off\">commit-on-save off</span> \
                · saves change files only; commit with git yourself</p>"
            .to_string();
    };
    let git = sync.git();
    let branch = match git.current_branch() {
        Ok(Some(b)) => git_text(&b),
        Ok(None) => "detached HEAD".to_string(),
        Err(e) => format!("branch unknown ({})", git_text(&e.to_string())),
    };
    let pending = match git.status() {
        Ok(s) => s.entries.len(),
        Err(_) => 0,
    };
    let publication = match git.divergence() {
        Ok(d) if !d.has_upstream => "no remote".to_string(),
        Ok(d) if d.ahead == 0 && d.behind == 0 => "in sync with remote".to_string(),
        Ok(d) => format!("{} ahead, {} behind", d.ahead, d.behind),
        Err(e) => format!("remote unknown ({})", git_text(&e.to_string())),
    };
    let pending_text = if pending == 0 {
        "working tree clean".to_string()
    } else {
        format!("{pending} uncommitted change(s) outside BerryWiki")
    };
    format!(
        "<p class=\"status-strip\"><span class=\"status-on\">commit-on-save</span> · \
         branch <code>{branch}</code> · {pending_text} · {publication} · \
         <a href=\"/changes\">Changes</a> · <a href=\"/conflicts\">Conflicts</a></p>"
    )
}

/// `/changes`: what is pending, what is published, recent commits, and the
/// Sync form. `error` renders a banner (used by `POST /sync` failures).
pub(crate) fn changes_page(ctx: Ctx<'_>, notice: &str, error: Option<&str>) -> Response {
    let mut main = String::new();
    let Some(sync) = ctx.sync else {
        let why = if ctx.editing {
            "This server was started with <code>--no-commit</code> (or the folder is not \
             a git working tree), so saves change files only. Commit and push with git \
             yourself."
        } else {
            "This is the read-only view; there is no repository to report on."
        };
        main.push_str(&format!(
            "<p class=\"notice\">Commit-on-save is off.</p><p>{why}</p>"
        ));
        return Response::html(200, layout(ctx, None, "Changes", main));
    };
    if let Some(n) = sync_notice_text(notice) {
        main.push_str(&format!("<p class=\"notice\">{n}</p>"));
    }
    if let Some(e) = error {
        main.push_str(&format!(
            "<p class=\"error-banner\">Sync failed: {}</p>",
            git_text(e)
        ));
    }
    let git = sync.git();

    // Pending (uncommitted) changes made outside BerryWiki.
    main.push_str("<h2>Pending changes</h2>");
    match git.status() {
        Ok(s) if s.is_clean() => {
            main.push_str("<p>Working tree clean — every change is committed.</p>")
        }
        Ok(s) => {
            main.push_str(
                "<p>These files changed outside BerryWiki. The next save or sync records \
                 them first as their own commit, <em>Record changes made outside \
                 BerryWiki</em>, so nothing is mixed into a page edit.</p><ul class=\"pending\">",
            );
            for e in &s.entries {
                main.push_str(&format!("<li><code>{}</code></li>", git_text(e)));
            }
            main.push_str("</ul>");
        }
        Err(e) => main.push_str(&format!(
            "<p class=\"error-banner\">Could not read the working tree: {}</p>",
            git_text(&e.to_string())
        )),
    }

    // Publication state.
    main.push_str("<h2>Publication</h2>");
    match git.divergence() {
        Ok(d) if !d.has_upstream => main.push_str(
            "<p>No remote is configured for this branch; commits stay local until you \
             add one.</p>",
        ),
        Ok(d) => main.push_str(&format!(
            "<p>{} local commit(s) not yet published; {} remote commit(s) not yet \
             integrated (as of the last fetch).</p>",
            d.ahead, d.behind
        )),
        Err(e) => main.push_str(&format!(
            "<p class=\"error-banner\">Could not compare with the remote: {}</p>",
            git_text(&e.to_string())
        )),
    }
    if let Some(o) = ctx.last_sync {
        main.push_str(&format!(
            "<p>Last sync in this session: {}</p>",
            escape_html(&sync_outcome_text(o))
        ));
    }
    if ctx.editing {
        main.push_str(
            "<form method=\"post\" action=\"/sync\"><div class=\"editor-buttons\">\
             <button type=\"submit\">Sync now</button></div>\
             <p class=\"hint\">Fetch, fast-forward if possible, then push. BerryWiki \
             never merges, rebases or force-pushes; if both sides moved it stops and \
             shows the <a href=\"/conflicts\">conflicts page</a>.</p></form>",
        );
    }

    // Recent history.
    main.push_str("<h2>Recent commits</h2>");
    match git.recent(50) {
        Ok(entries) if entries.is_empty() => main.push_str("<p>No commits yet.</p>"),
        Ok(entries) => {
            main.push_str("<ol class=\"commits\">");
            for e in &entries {
                main.push_str(&format!(
                    "<li><code>{}</code> {} <span class=\"when\">{}</span></li>",
                    git_text(e.id.short()),
                    git_text(&e.subject),
                    git_text(&e.date)
                ));
            }
            main.push_str("</ol>");
        }
        Err(e) => main.push_str(&format!(
            "<p class=\"error-banner\">Could not read history: {}</p>",
            git_text(&e.to_string())
        )),
    }
    Response::html(200, layout(ctx, None, "Changes", main))
}

/// `/conflicts` has two states. When a merge someone else started is
/// unfinished in the wiki folder, this is the conflict view: every unsettled
/// path, what kind of clash it is, and the three sides side by side. When no
/// merge is under way it is the diverged hand-off, written for a person to
/// settle with git, because BerryWiki never begins a merge of its own
/// (ADR-0010).
fn conflicts_page(ctx: Ctx<'_>) -> Response {
    let Some(sync) = ctx.sync else {
        let main = "<p class=\"notice\">Commit-on-save is off, so BerryWiki tracks no \
                    remote and reports no conflicts.</p>"
            .to_string();
        return Response::html(200, layout(ctx, None, "Conflicts", main));
    };
    match sync.conflicts() {
        Ok(Some(report)) => {
            let main = render_merge(&report);
            return Response::html(200, layout(ctx, None, "Conflicts", main));
        }
        Ok(None) => {}
        Err(e) => {
            let main = format!(
                "<p class=\"error-banner\">Could not read the unfinished merge: {}</p>",
                git_text(&e.to_string())
            );
            return Response::html(200, layout(ctx, None, "Conflicts", main));
        }
    }
    let main = match sync.diverged() {
        Ok(None) => "<p>No conflict: the local branch and its remote have not both moved \
                     since they last agreed.</p>\
                     <p><a href=\"/changes\">Back to changes</a></p>"
            .to_string(),
        Ok(Some(h)) => render_handoff(&h),
        Err(e) => format!(
            "<p class=\"error-banner\">Could not compare with the remote: {}</p>",
            git_text(&e.to_string())
        ),
    };
    Response::html(200, layout(ctx, None, "Conflicts", main))
}

/// How much of one side of a clash the page shows. A wiki page can be far
/// longer than anyone will read in a three-way comparison, and the job here is
/// to recognise the clash, not to settle it in a browser.
const CONFLICT_EXCERPT: usize = 4000;

/// One side of one conflict, escaped and redacted, shortened if it is long.
fn conflict_excerpt(text: &str) -> String {
    let kept: String = text.chars().take(CONFLICT_EXCERPT).collect();
    let mut html = git_text(&kept);
    if text.chars().count() > CONFLICT_EXCERPT {
        html.push_str("\n… shortened here; the whole file is in the wiki folder.");
    }
    html
}

/// The unfinished-merge view: what clashed, what BerryWiki settled by itself,
/// and either the offer to conclude or the instructions to settle the rest.
fn render_merge(report: &ConflictReport) -> String {
    let mut main = String::new();
    main.push_str(
        "<p class=\"error-banner\">A merge is unfinished in the wiki folder. BerryWiki did \
         not start it and will not decide it: nothing below has been changed, and saving a \
         page is refused until the merge is concluded.</p>",
    );
    if report.files.is_empty() {
        main.push_str("<p>Nothing is unsettled; the merge is ready to conclude.</p>");
    } else {
        main.push_str("<table class=\"conflicts\"><tr><th>File</th><th>What happened</th></tr>");
        for f in &report.files {
            main.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{}{}</td></tr>",
                git_text(&f.path),
                escape_html(f.kind.describe()),
                if f.kind.is_auto_resolvable() {
                    " <span class=\"auto\">settled by regeneration</span>"
                } else {
                    ""
                }
            ));
        }
        main.push_str("</table>");
    }
    for f in report.blocking() {
        main.push_str(&format!(
            "<h2><code>{}</code></h2><p>{}</p>",
            git_text(&f.path),
            escape_html(f.kind.describe())
        ));
        for (label, side) in [
            ("Common ancestor", &f.base),
            ("Here", &f.ours),
            ("Remote", &f.theirs),
        ] {
            match side {
                Some(text) => main.push_str(&format!(
                    "<h3>{label}</h3><pre class=\"side\">{}</pre>",
                    conflict_excerpt(text)
                )),
                None => main.push_str(&format!(
                    "<h3>{label}</h3><p class=\"absent\">Not present on this side.</p>"
                )),
            }
        }
    }
    if report.can_finish() {
        main.push_str(
            "<form method=\"post\" action=\"/conflicts/finish\">\
             <p>Nothing is left for a person to decide. Concluding the merge regenerates the \
             navigation file from the merged pages and records one merge commit.</p>\
             <button type=\"submit\">Conclude the merge</button></form>",
        );
    } else {
        main.push_str(
            "<p>Settle the files above in the wiki folder, then reload this page. BerryWiki \
             will not choose between two people's words.</p>\
             <pre>git status\n# edit each file until only the wanted text remains, then\ngit add &lt;file&gt;</pre>\
             <p>Once nothing is left unsettled, this page offers to conclude the merge.</p>",
        );
    }
    main.push_str("<p><a href=\"/changes\">Back to changes</a></p>");
    main
}

fn render_handoff(h: &DivergedHandoff) -> String {
    format!(
        "<p class=\"error-banner\">Local and remote have diverged: {ahead} local commit(s) \
         and {behind} remote commit(s) since their common ancestor.</p>\
         <table class=\"handoff\"><tr><th>Local tip</th><td><code>{local}</code></td></tr>\
         <tr><th>Remote tip</th><td><code>{upstream}</code></td></tr>\
         <tr><th>Common base</th><td><code>{base}</code></td></tr></table>\
         <p>BerryWiki has fetched the remote but will not merge, rebase or force-push \
         (ADR-0010): your pages and the remote's are both intact and the working tree \
         is at the local tip. Resolve with git in the wiki folder:</p>\
         <pre>git fetch\ngit merge @{{u}}      # or: git rebase @{{u}}\n# resolve any conflicts, then\ngit push</pre>\
         <p>Then <a href=\"/changes\">sync again</a>. If the merge touched the tree, \
         regenerate the sidebar with <code>berrywiki sidebar</code> before pushing so \
         the native GitHub reader stays consistent.</p>",
        ahead = h.ahead,
        behind = h.behind,
        local = git_text(h.local.short()),
        upstream = git_text(h.upstream.short()),
        base = git_text(h.base.short()),
    )
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

fn search_page(ctx: Ctx<'_>, q: &str, tag: &str) -> Response {
    let needle = q.trim().to_lowercase();
    let tag = tag.trim();
    let main = if needle.is_empty() && tag.is_empty() {
        "<p>Type a query above.</p>".to_string()
    } else {
        let mut hits = Vec::new();
        for page in ctx.store.graph().pages() {
            // The tag is a filter, not a ranking signal: a page without it is
            // not a weaker result, it is not a result.
            if !tag.is_empty() && !has_tag(page, tag) {
                continue;
            }
            let in_title = page.title.to_lowercase().contains(&needle);
            let in_body = page.body.to_lowercase().contains(&needle);
            // An empty query with a tag filter lists everything carrying it,
            // which is what makes `/search?tag=x` a usable link on its own.
            if needle.is_empty() || in_title || in_body {
                hits.push(format!(
                    "<li><a href=\"/page/{}\">{}</a>{}</li>",
                    escape_attr(&page.id),
                    escape_html(&page.title),
                    if in_title || needle.is_empty() {
                        ""
                    } else {
                        " <small>(body)</small>"
                    }
                ));
            }
        }
        let what = match (needle.is_empty(), tag.is_empty()) {
            (false, false) => format!("“{}” tagged “{}”", escape_html(q.trim()), escape_html(tag)),
            (false, true) => format!("“{}”", escape_html(q.trim())),
            (true, _) => format!("tag “{}”", escape_html(tag)),
        };
        if hits.is_empty() {
            format!("<p>No pages match {what}.</p>")
        } else {
            format!(
                "<p>{} result(s) for {what}:</p><ul class=\"results\">{}</ul>",
                hits.len(),
                hits.join("")
            )
        }
    };
    Response::html(200, layout(ctx, None, "Search", main))
}

/// Exact, case-sensitive tag membership.
///
/// Case-folding here would make `/tags/Rust` and `/tags/rust` disagree with
/// the tag list rendered on the page itself, which is stored verbatim.
fn has_tag(page: &berrywiki_core::WikiPage, tag: &str) -> bool {
    page.metadata
        .as_ref()
        .is_some_and(|m| m.tags.iter().any(|t| t == tag))
}

/// `/tags` — every tag in the wiki with the number of pages carrying it.
///
/// A `BTreeMap` rather than a `HashMap` so the listing is byte-deterministic,
/// the same rule the sidebar generator follows.
fn tags_index_page(ctx: Ctx<'_>) -> Response {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for page in ctx.store.graph().pages() {
        if let Some(meta) = &page.metadata {
            for tag in &meta.tags {
                *counts.entry(tag.as_str()).or_insert(0) += 1;
            }
        }
    }
    let main = if counts.is_empty() {
        "<p>No page carries a tag yet.</p>".to_string()
    } else {
        let items: String = counts
            .iter()
            .map(|(tag, n)| {
                format!(
                    "<li><a class=\"tag\" href=\"/tags/{}\">{}</a> <small>({n})</small></li>",
                    escape_attr(&percent_encode(tag)),
                    escape_html(tag)
                )
            })
            .collect();
        format!("<ul class=\"tag-index\">{items}</ul>")
    };
    Response::html(200, layout(ctx, None, "Tags", main))
}

/// `/tags/<tag>` — the pages carrying one tag, in graph order.
fn tag_page(ctx: Ctx<'_>, tag: &str) -> Response {
    let hits: String = ctx
        .store
        .graph()
        .pages()
        .iter()
        .filter(|p| has_tag(p, tag))
        .map(|p| {
            format!(
                "<li><a href=\"/page/{}\">{}</a></li>",
                escape_attr(&p.id),
                escape_html(&p.title)
            )
        })
        .collect();
    let shown = escape_html(tag);
    let main = if hits.is_empty() {
        format!("<p>No page is tagged “{shown}”. <a href=\"/tags\">All tags</a></p>")
    } else {
        format!(
            "<p>Pages tagged “{shown}”:</p><ul class=\"results\">{hits}</ul>\
             <p><a href=\"/tags\">All tags</a></p>"
        )
    };
    Response::html(200, layout(ctx, None, &format!("Tag — {tag}"), main))
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
                // Three distinct contexts, three distinct encoders: the path
                // segment is percent-encoded, the attribute is attribute-
                // escaped, the visible text is HTML-escaped. A tag containing
                // `/`, `?` or `"` is safe only because each slot gets its own.
                out.push_str(&format!(
                    "<a class=\"tag\" href=\"/tags/{}\">{}</a> ",
                    escape_attr(&percent_encode(tag)),
                    escape_html(tag)
                ));
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
    let strip = status_strip(ctx);
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title} — BerryWiki</title><style>{CSS}</style></head><body>\
<header class=\"topbar\"><a class=\"brand\" href=\"/\">BerryWiki</a>{new_link}\
<form class=\"search\" method=\"get\" action=\"/search\" role=\"search\">\
<input type=\"search\" name=\"q\" placeholder=\"Search…\" aria-label=\"Search\">\
<button type=\"submit\">Search</button></form>\
<a class=\"diag-link\" href=\"/diagnostics\">Diagnostics</a></header>{strip}\
<div class=\"grid\">{nav}<main class=\"main\"><h1>{title_h1}</h1>{main}</main>{aside_html}</div>\
</body></html>",
        title = escape_html(title),
        title_h1 = escape_html(title),
        new_link = new_link,
        strip = strip,
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
a.tag{text-decoration:none}\
a.tag:hover,a.tag:focus{background:#e2c8cc;text-decoration:underline}\
.tag-index{list-style:none;padding:0}\
.tag-index li{padding:.15rem 0}\
.field-hint{margin:.25rem 0 0;font-size:.8rem;color:#666}\
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
.status-strip{margin:0;padding:.3rem 1rem;font-size:.85rem;background:#f6f1f2;border-bottom:1px solid #e5e5e5;color:#444}\
.status-on{color:#2e7d32;font-weight:600}.status-off{color:#7a5b00;font-weight:600}\
.commits{padding-left:1.4rem}.commits .when{color:#888;font-size:.85rem}\
.pending{padding-left:1.4rem}.hint{font-size:.85rem;color:#555}\
.handoff th{text-align:left;padding-right:1rem}\
.plan{border-collapse:collapse;margin:.5rem 0}.plan th,.plan td{text-align:left;padding:.2rem .8rem .2rem 0;vertical-align:top}\
@media(prefers-color-scheme:dark){.status-strip{background:#1d1d1d;border-color:#333;color:#cfcfcf}body{background:#161616;color:#e6e6e6}.tree,.context{border-color:#333}.tree-item a{color:#cfcfcf}.tree-item a:hover{background:#222}.page pre{background:#222}\
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
            let hex_str = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex_str, 16) {
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

/// Percent-encode a string for use as a single URL path segment.
///
/// The RFC 3986 unreserved set (`A-Z a-z 0-9 - . _ ~`) passes through; every
/// other byte becomes `%XX`. Hand-rolled rather than pulled from a crate
/// because the dependency budget is one third-party crate (comrak) and this is
/// twenty lines. It is the exact inverse of [`percent_decode`], pinned by
/// `percent_encode_round_trips_through_percent_decode`.
pub(crate) fn percent_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
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

    /// The `<ul class="results">` fragment alone.
    ///
    /// Every page renders the whole tree in its sidebar, so an absence
    /// assertion against the full body would be about the layout rather than
    /// about the filter under test, and would pass for the wrong reason.
    fn results_only(html: &str) -> &str {
        match html.split_once("<ul class=\"results\">") {
            Some((_, rest)) => rest.split_once("</ul>").map_or(rest, |(a, _)| a),
            None => "",
        }
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
        assert!(
            !r.body.contains("/move"),
            "read-only view must not offer Move"
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
            ("/changes", ""),
            ("/changes", "notice=synced-published"),
            ("/changes", "notice=merge-finished"),
            ("/conflicts", ""),
            (&format!("/page/{HOME_ID}"), ""),
            (&format!("/page/{HOME_ID}"), "notice=committed"),
            ("/page/missing", ""),
            // P4-tags. A new route that is not listed here is not swept, and
            // the sweep still calls itself "every route", so the list grows
            // with the router or the guarantee quietly stops covering it.
            ("/tags", ""),
            ("/tags/teaching", ""),
            ("/tags/nobody-uses-this", ""),
            ("/tags/%22%3E%3Cscript%3E", ""),
            ("/search", "tag=teaching"),
            ("/search", "q=e&tag=teaching"),
            ("/search", "tag=%22%3E%3Cscript%3Ealert(1)%3C/script%3E"),
        ] {
            let r = route(&s, path, query);
            // Same reason as the synced sweep: an absent route 404s, and a 404
            // is script-free, so without this the sweep passes vacuously.
            let expected = if path == "/page/missing" { 404 } else { 200 };
            assert_eq!(r.status, expected, "{path}?{query}");
            no_script(&r.body);
        }
    }

    #[test]
    fn read_only_route_never_renders_git_affordances() {
        let s = store();
        for path in ["/changes", "/conflicts", &format!("/page/{HOME_ID}")] {
            let body = route(&s, path, "").body;
            assert!(
                !body.contains("class=\"status-strip\""),
                "{path} shows a status strip"
            );
            assert!(
                !body.contains("action=\"/sync\""),
                "{path} shows a sync form"
            );
        }
        // The plain-mode explanation is honest about why.
        assert!(route(&s, "/changes", "")
            .body
            .contains("Commit-on-save is off"));
    }

    #[test]
    fn redact_secret_masks_only_the_configured_token() {
        // Without the env var set, text passes through unchanged.
        if std::env::var("BERRYWIKI_GITHUB_TOKEN").is_err() {
            assert_eq!(redact_secret("ghp_abc"), "ghp_abc");
        }
    }

    #[test]
    fn percent_encode_round_trips_through_percent_decode() {
        // `percent_encode`'s doc comment claims to be the exact inverse of
        // `percent_decode`. This is the test that makes that claim true rather
        // than decorative; the tag routes rely on it for every non-ASCII or
        // punctuated tag.
        for s in [
            "",
            "teaching",
            "course-a",
            "with space",
            "slash/and?query",
            "quote\"and<angle>",
            "percent%25already",
            "unicode-\u{e9}\u{4e2d}\u{6587}",
            "plus+and&amp",
            "-._~",
        ] {
            assert_eq!(percent_decode(&percent_encode(s)), s, "round trip {s:?}");
        }
    }

    #[test]
    fn percent_encode_passes_the_unreserved_set_through_untouched() {
        // RFC 3986 unreserved. If this set ever shrinks, every existing tag
        // link changes shape silently, so it is pinned rather than inferred.
        let unreserved = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
        assert_eq!(percent_encode(unreserved), unreserved);
        assert_eq!(percent_encode("/"), "%2F");
        assert_eq!(percent_encode(" "), "%20");
    }

    #[test]
    fn tags_index_counts_pages_per_tag_and_links_each_one() {
        let s = store();
        let r = route(&s, "/tags", "");
        assert_eq!(r.status, 200);
        // `teaching` is on three fixture pages; the singletons prove the count
        // is per-tag rather than a total.
        assert!(
            r.body.contains("href=\"/tags/teaching\""),
            "tag is a link: {}",
            r.body
        );
        assert!(r.body.contains("(3)"), "teaching counted three times");
        assert!(r.body.contains("href=\"/tags/course-a\""));
        assert!(r.body.contains("href=\"/tags/assessment\""));
        no_script(&r.body);
    }

    #[test]
    fn tag_page_lists_exactly_the_pages_carrying_that_tag() {
        let s = store();
        let r = route(&s, "/tags/teaching", "");
        assert_eq!(r.status, 200);
        // Positive control first: if the extraction returned an empty fragment
        // the absence assertion below would pass for the wrong reason.
        let hits = results_only(&r.body);
        assert!(hits.contains("Assessment Plan"), "results: {hits}");
        assert!(hits.contains("Course A"));
        // A page tagged `research` is not tagged `teaching`, so it must be
        // absent: the tag is a filter, not a ranking signal.
        assert!(
            !results_only(&r.body).contains(">Research<"),
            "untagged page leaked into the results"
        );
        no_script(&r.body);
    }

    #[test]
    fn tag_page_is_case_sensitive() {
        // Case-folding here would disagree with the verbatim tag list rendered
        // on the page itself, so `Teaching` is a different tag from `teaching`.
        let s = store();
        assert!(route(&s, "/tags/Teaching", "")
            .body
            .contains("No page is tagged"));
    }

    #[test]
    fn tag_page_for_an_unknown_tag_is_an_empty_result_not_an_error() {
        let s = store();
        let r = route(&s, "/tags/nobody-uses-this", "");
        assert_eq!(r.status, 200, "an absent tag is not a missing page");
        assert!(r.body.contains("No page is tagged"));
        assert!(r.body.contains("href=\"/tags\""), "offers a way back");
    }

    #[test]
    fn tag_page_escapes_a_hostile_tag_in_every_context() {
        let s = store();
        let hostile = "\"><script>alert(1)</script>";
        let r = route(&s, &format!("/tags/{}", percent_encode(hostile)), "");
        assert_eq!(r.status, 200);
        no_script(&r.body);
        // The visible text is HTML-escaped, so the angle brackets survive as
        // entities rather than as markup.
        assert!(r.body.contains("&lt;script&gt;"), "escaped, not stripped");
    }

    #[test]
    fn search_filters_by_tag_alone_and_alongside_a_query() {
        let s = store();
        // A tag with no query lists everything carrying it, which is what makes
        // `/search?tag=x` a usable link on its own.
        let only_tag = route(&s, "/search", "tag=teaching");
        assert_eq!(only_tag.status, 200);
        assert!(results_only(&only_tag.body).contains("Assessment Plan"));
        assert!(!results_only(&only_tag.body).contains(">Research<"));
        // Adding a query narrows further; it never widens.
        let both = route(&s, "/search", "q=assessment&tag=teaching");
        assert!(both.body.contains("Assessment Plan"));
        // A tag nothing carries yields no results even when the query matches.
        let impossible = route(&s, "/search", "q=e&tag=nobody-uses-this");
        assert!(
            impossible.body.contains("No pages match"),
            "tag filter is not advisory"
        );
    }

    #[test]
    fn search_with_neither_query_nor_tag_still_prompts() {
        let s = store();
        assert!(route(&s, "/search", "")
            .body
            .contains("Type a query above."));
    }

    #[test]
    fn page_view_renders_its_tags_as_links() {
        let s = store();
        let r = route(&s, &format!("/page/{PLAN_ID}"), "");
        assert!(
            r.body.contains("href=\"/tags/assessment\""),
            "tags in the context pane are navigable, not inert spans"
        );
        no_script(&r.body);
    }
}
