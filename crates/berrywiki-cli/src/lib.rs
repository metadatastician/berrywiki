// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! The `berrywiki` CLI, as a testable library.
//!
//! `main.rs` is a thin shell over [`run`], which takes its arguments and an
//! output sink explicitly so every command can be exercised in-process with no
//! subprocess spawning.
//!
//! Commands (Phase 1 tooling; not the product UI):
//! * `berrywiki check <folder>` — load a wiki folder and print its tree,
//!   diagnostics and a summary; exit non-zero if any *error*-level diagnostic
//!   is present (so it can gate CI, like a linter).
//! * `berrywiki sidebar <folder> [--write]` — print the deterministically
//!   generated `_Sidebar.md`, or (with `--write`) regenerate it in place.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

mod import;

use berrywiki_appstate::{LockError, RepoLock};
use berrywiki_core::{generate_sidebar, Severity, SidebarOptions};
use berrywiki_store::{LocalFolderStore, WikiStore};

const USAGE: &str = "\
berrywiki — inspect and maintain a wiki folder

USAGE:
    berrywiki check <folder>
    berrywiki sidebar <folder> [--write]
    berrywiki serve <folder> [--addr 127.0.0.1:23779] [--no-commit]
                             [--author \"Name <email>\"]
    berrywiki serve --github <owner/repo> [--cache dir] [--addr host:port]
    berrywiki backup <folder> <out-dir>
    berrywiki restore <backup-dir> <folder>
    berrywiki import <notebook.ctd> <folder> [--apply] [--json]
    berrywiki --help

COMMANDS:
    check      Load the wiki and print its tree + diagnostics. Exit code 1 if
               any error-level diagnostic is found, else 0.
    sidebar    Print the generated _Sidebar.md, or regenerate it with --write.
    serve      Start a zero-JavaScript web explorer and editor (three-pane:
               tree | page | outline/backlinks; edit/create/delete with explicit
               Save and Save-draft). A local folder that is a git working tree
               is served with commit-on-save: every save is one commit, sidebar
               included, and /changes offers fetch + fast-forward + push (never
               a merge, never a force). --no-commit serves the folder without
               touching git; a folder that is not a git working tree falls back
               to that with a warning. --author sets the commit identity
               (default: the git config of the clone). A GitHub mirror via
               --github is read-only (token via BERRYWIKI_GITHUB_TOKEN for
               private wikis). Listens on TCP only, loopback by default, port
               23779 (IANA-unassigned). Blocks until interrupted.
    backup     Write a recoverable copy of the wiki to a new directory: a git
               bundle of all committed history, plus drafts and the operation
               journal. Refuses a dirty working tree, because a bundle carries
               committed history only and would silently omit the rest. The
               search index is not archived; it is derived data.
    restore    Rebuild a wiki from a backup directory into a new folder, set
               its remote from the recorded origin, and put the drafts back
               under the new folder's own app state. Refuses a folder that
               already has contents.
    import     Read a CherryTree notebook and report what it would become.
               Writes nothing without --apply, and even then refuses rather
               than overwrite: a title shared by two siblings, a page that
               already exists and was not imported from this same node, or a
               folder that is not a git working tree all stop the run before
               the first write. An applied import is one commit. Re-importing
               the same notebook writes nothing, because every imported page
               records where it came from. --json emits the same report as
               machine-readable data.
";

/// Flags that take a value, so `first_path` never mistakes the value for the
/// folder (`berrywiki serve --author \"A <a@b>\" ./wiki` must serve ./wiki).
const VALUE_FLAGS: &[&str] = &["--addr", "--github", "--cache", "--author"];

/// Default listen address. 23779 is in the IANA-unassigned block
/// 23547–23999 (checked 2026-09-02 against the service-names registry) and is
/// deliberately far from the 8080/3000-class ports that collide constantly.
pub const DEFAULT_ADDR: &str = "127.0.0.1:23779";

/// Run the CLI. Returns the process exit code. All output (including error
/// messages) goes to `out`; nothing is printed to real stdout/stderr here.
pub fn run(args: &[String], out: &mut dyn Write) -> io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("check") => cmd_check(first_path(&args[1..]), out),
        Some("sidebar") => {
            cmd_sidebar(first_path(&args[1..]), has_flag(&args[1..], "--write"), out)
        }
        Some("serve") => cmd_serve(&args[1..], out),
        Some("backup") => cmd_backup(&args[1..], out),
        Some("import") => import::cmd_import(&args[1..], out),
        Some("restore") => cmd_restore(&args[1..], out),
        Some("--help") | Some("-h") | Some("help") | None => {
            write!(out, "{USAGE}")?;
            Ok(0)
        }
        Some(other) => {
            writeln!(out, "unknown command: {other:?}\n")?;
            write!(out, "{USAGE}")?;
            Ok(2)
        }
    }
}

/// First positional (non-`--`) argument, if any.
fn first_path(args: &[String]) -> Option<&str> {
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            skip_next = VALUE_FLAGS.contains(&a.as_str());
            continue;
        }
        return Some(a.as_str());
    }
    None
}

/// Parse `--author "Name <email>"` into a git identity; `None` if absent or
/// malformed (the caller reports malformed input rather than guessing).
fn parse_author(raw: &str) -> Option<berrywiki_git::Identity> {
    let raw = raw.trim();
    let open = raw.rfind('<')?;
    let close = raw.rfind('>')?;
    if close < open || !raw.ends_with('>') {
        return None;
    }
    let name = raw[..open].trim();
    let email = raw[open + 1..close].trim();
    if name.is_empty() || email.is_empty() || !email.contains('@') {
        return None;
    }
    Some(berrywiki_git::Identity {
        name: name.to_string(),
        email: email.to_string(),
    })
}

pub(crate) fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// Value following `--flag` (e.g. `--addr 127.0.0.1:9000`), if present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn cmd_check(path: Option<&str>, out: &mut dyn Write) -> io::Result<i32> {
    let Some(path) = path else {
        writeln!(out, "usage: berrywiki check <folder>")?;
        return Ok(2);
    };
    let store = match LocalFolderStore::open(path) {
        Ok(s) => s,
        Err(e) => {
            writeln!(out, "error: {e}")?;
            return Ok(2);
        }
    };

    let pages = store.list_pages();
    writeln!(out, "{} page(s) in {path}", pages.len())?;
    writeln!(out)?;

    // Tree (deterministic pre-order).
    for (depth, page) in store.graph().walk() {
        let marker = if page.is_archived() {
            " (archived)"
        } else {
            ""
        };
        writeln!(out, "{}- {}{marker}", "  ".repeat(depth), page.title)?;
    }

    // Diagnostics: graph consistency + load-time (skipped files).
    let mut errors = 0usize;
    let mut warnings = 0usize;
    let diags: Vec<_> = store
        .graph()
        .diagnostics()
        .iter()
        .chain(store.load_diagnostics().iter())
        .collect();
    if !diags.is_empty() {
        writeln!(out, "\ndiagnostics:")?;
        for d in &diags {
            match d.severity {
                Severity::Error => errors += 1,
                Severity::Warning => warnings += 1,
                Severity::Info => {}
            }
            writeln!(out, "  {d}")?;
        }
    }

    writeln!(out, "\n{errors} error(s), {warnings} warning(s)")?;
    Ok(if errors > 0 { 1 } else { 0 })
}

/// Result of asking for the single-writer lock before a mutation.
pub(crate) enum WriterLock {
    /// We are the wiki's only writer for as long as the value lives.
    Held(RepoLock),
    /// App state could not be resolved, so no lock exists; the caller warns
    /// and continues, exactly as drafts degrade (ADR-0008).
    Unavailable,
    /// Another writer holds the wiki; the caller has printed why and exits 2.
    Refused,
}

/// Take the single-writer lock (X-lock) or say why not. Never waits: a second
/// `serve`, or a CLI write while a server runs, is refused naming the holder.
pub(crate) fn writer_lock(
    store: &LocalFolderStore,
    label: &str,
    path: &str,
    out: &mut dyn Write,
) -> io::Result<WriterLock> {
    let Some(state) = store.appstate() else {
        writeln!(
            out,
            "warning: app state for {path} could not be resolved; single-writer lock unavailable"
        )?;
        return Ok(WriterLock::Unavailable);
    };
    match RepoLock::acquire(state, label) {
        Ok(lock) => Ok(WriterLock::Held(lock)),
        Err(e @ LockError::Held { .. }) => {
            writeln!(
                out,
                "error: {path}: {e}; only one writer may open a wiki at a time"
            )?;
            Ok(WriterLock::Refused)
        }
        Err(e) => {
            writeln!(out, "error: {e}")?;
            Ok(WriterLock::Refused)
        }
    }
}

fn cmd_sidebar(path: Option<&str>, write: bool, out: &mut dyn Write) -> io::Result<i32> {
    let Some(path) = path else {
        writeln!(out, "usage: berrywiki sidebar <folder> [--write]")?;
        return Ok(2);
    };

    if write {
        let mut store = match LocalFolderStore::open(path) {
            Ok(s) => s,
            Err(e) => {
                writeln!(out, "error: {e}")?;
                return Ok(2);
            }
        };
        let _lock = match writer_lock(&store, "sidebar--write", path, out)? {
            WriterLock::Refused => return Ok(2),
            WriterLock::Held(l) => Some(l),
            WriterLock::Unavailable => None,
        };
        match store.regenerate_sidebar() {
            Ok(true) => writeln!(out, "_Sidebar.md updated")?,
            Ok(false) => writeln!(out, "_Sidebar.md already up to date")?,
            Err(e) => {
                writeln!(out, "error: {e}")?;
                return Ok(2);
            }
        }
        Ok(0)
    } else {
        let store = match LocalFolderStore::open(path) {
            Ok(s) => s,
            Err(e) => {
                writeln!(out, "error: {e}")?;
                return Ok(2);
            }
        };
        let sidebar = generate_sidebar(store.graph(), &SidebarOptions::default());
        write!(out, "{sidebar}")?;
        Ok(0)
    }
}

fn cmd_serve(args: &[String], out: &mut dyn Write) -> io::Result<i32> {
    let addr = flag_value(args, "--addr").unwrap_or(DEFAULT_ADDR);

    // `--github <owner/repo|url>` mirrors the wiki into a cache dir and serves
    // it; otherwise a local folder is served directly.
    if let Some(repo) = flag_value(args, "--github") {
        let cache = match flag_value(args, "--cache") {
            Some(c) => std::path::PathBuf::from(c),
            None => default_mirror_dir(repo),
        };
        // Token from the environment only — never a CLI arg (avoids logs/history).
        let token = std::env::var("BERRYWIKI_GITHUB_TOKEN").ok();
        let wiki = match berrywiki_github::GitHubWiki::open(repo, &cache, token.as_deref()) {
            Ok(w) => w,
            Err(e) => {
                writeln!(out, "error: {e}")?;
                return Ok(2);
            }
        };
        writeln!(
            out,
            "BerryWiki: mirrored {repo} into {}; serving at http://{addr}  (read-only; Ctrl-C to stop)",
            cache.display()
        )?;
        out.flush()?;
        return match berrywiki_serve::serve_readonly(wiki.store(), addr) {
            Ok(()) => Ok(0),
            Err(e) => {
                writeln!(out, "server error: {e}")?;
                Ok(2)
            }
        };
    }

    let Some(path) = first_path(args) else {
        writeln!(
            out,
            "usage: berrywiki serve <folder> [--addr host:port] [--no-commit] [--author \"Name <email>\"]\n       berrywiki serve --github <owner/repo> [--cache dir] [--addr host:port]"
        )?;
        return Ok(2);
    };
    let identity = match flag_value(args, "--author") {
        Some(raw) => match parse_author(raw) {
            Some(i) => Some(i),
            None => {
                writeln!(out, "error: --author must look like \"Name <email>\"")?;
                return Ok(2);
            }
        },
        None => None,
    };
    let store = match LocalFolderStore::open(path) {
        Ok(s) => s,
        Err(e) => {
            writeln!(out, "error: {e}")?;
            return Ok(2);
        }
    };

    // Single-writer lock (ADR-0008, X-lock): held for the life of this server.
    // A second `serve`, or a CLI write, is refused while we run.
    let lock = match writer_lock(&store, "serve", path, out)? {
        WriterLock::Refused => return Ok(2),
        WriterLock::Held(l) => Some(l),
        WriterLock::Unavailable => None,
    };

    // Commit-on-save (ADR-0010) unless opted out. A folder outside any git
    // working tree degrades to plain mode with a warning; any other git
    // failure is an error, because silently serving without commits would
    // hide that saves are not being recorded.
    let mut app = if has_flag(args, "--no-commit") {
        berrywiki_serve::App::new(store)
    } else {
        match berrywiki_git::GitRepo::open(path) {
            Ok(git) => {
                let git = match identity {
                    Some(i) => git.with_identity(i),
                    None => git,
                };
                berrywiki_serve::App::synced(berrywiki_sync::SyncedStore::new(store, git))
            }
            Err(berrywiki_git::GitError::NotARepo(_)) => {
                writeln!(
                    out,
                    "warning: {path} is not inside a git working tree; serving without commit-on-save"
                )?;
                berrywiki_serve::App::new(store)
            }
            Err(e) => {
                writeln!(out, "error: {e}")?;
                return Ok(2);
            }
        }
    };
    let drafts_note = if app.store().appstate().is_some() {
        "drafts on"
    } else {
        "drafts unavailable"
    };
    let lock_note = if lock.is_some() {
        "single-writer lock on"
    } else {
        "single-writer lock unavailable"
    };
    let commit_note = if app.commits_on_save() {
        "commit-on-save on"
    } else {
        "commit-on-save off"
    };
    writeln!(
        out,
        "BerryWiki: serving {path} at http://{addr}  (editable; {commit_note}; {drafts_note}; {lock_note}; Ctrl-C to stop)"
    )?;
    out.flush()?;
    match berrywiki_serve::serve(&mut app, addr) {
        Ok(()) => Ok(0),
        Err(e) => {
            writeln!(out, "server error: {e}")?;
            Ok(2)
        }
    }
}

/// Default mirror cache directory, keyed by repo and kept OUTSIDE any wiki
/// clone (per the "app state not inside the clone" rule). Uses XDG cache when
/// available, else the system temp dir.
fn default_mirror_dir(repo: &str) -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let slug: String = repo
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    base.join("berrywiki").join("mirrors").join(slug)
}

// ---------------------------------------------------------------------------
// backup / restore (ADR-0013)
// ---------------------------------------------------------------------------

/// First line of a backup's `MANIFEST`. A directory without it is not a
/// BerryWiki backup and is refused rather than half-read.
const BACKUP_MAGIC: &str = "berrywiki-backup: 1";

/// Positional arguments, in order, with flags and their values skipped.
///
/// `first_path` answers the one-positional commands; `backup` and `restore`
/// each take two, and mixing them up would write over the wrong directory.
fn positionals(args: &[String]) -> Vec<&str> {
    let mut found = Vec::new();
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with("--") {
            skip_next = VALUE_FLAGS.contains(&a.as_str());
            continue;
        }
        found.push(a.as_str());
    }
    found
}

/// Is `p` absent, or an existing directory with nothing in it?
///
/// Both `backup` and `restore` write a whole directory, and the rule against
/// discarding work the user already has means neither may write into one that
/// holds anything.
fn vacant_dir(p: &Path) -> io::Result<bool> {
    match fs::read_dir(p) {
        Ok(mut entries) => Ok(entries.next().is_none()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

/// Copy a directory tree. Files and directories only: a symlink is followed
/// and copied as its target, which is what drafts hold in practice, and a
/// device or socket would fail loudly rather than be silently skipped.
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Strip any `user:password@` from a remote URL before it is written down.
///
/// A backup is a file that gets copied to other machines. An origin URL with
/// embedded credentials would turn "keep a copy of your wiki" into "publish
/// your password", so the userinfo never reaches the manifest. The URL is
/// still useful without it: git prompts, or the credential helper answers.
fn strip_userinfo(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        // scp-style `git@host:path` carries no password; leave it alone.
        return url.to_string();
    };
    let (scheme, rest) = url.split_at(scheme_end + 3);
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{scheme}{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// Read a `key: value` manifest into a lookup list, in file order.
fn parse_manifest(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_end();
            let (k, v) = line.split_once(':')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn manifest_value<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// `berrywiki backup <folder> <out-dir>`.
fn cmd_backup(args: &[String], out: &mut dyn Write) -> io::Result<i32> {
    let paths = positionals(args);
    let (Some(wiki), Some(dest)) = (paths.first(), paths.get(1)) else {
        writeln!(out, "usage: berrywiki backup <folder> <out-dir>")?;
        return Ok(2);
    };
    let wiki = Path::new(wiki);
    let dest = Path::new(dest);

    if !vacant_dir(dest)? {
        writeln!(
            out,
            "error: {} already has contents; backup writes a whole directory and will not \
             write into one that holds anything",
            dest.display()
        )?;
        return Ok(2);
    }

    let store = match LocalFolderStore::open(wiki) {
        Ok(s) => s,
        Err(e) => {
            writeln!(out, "error: {e}")?;
            return Ok(2);
        }
    };

    // Held for the whole backup so drafts cannot be half-written into it.
    // A torn draft is an incorrect backup, not merely an untidy one.
    let _lock = match writer_lock(&store, "backup", &wiki.display().to_string(), out)? {
        WriterLock::Refused => return Ok(2),
        WriterLock::Held(l) => Some(l),
        WriterLock::Unavailable => None,
    };

    let repo = match berrywiki_git::GitRepo::open(wiki) {
        Ok(r) => r,
        Err(e) => {
            writeln!(
                out,
                "error: {e}; backup archives committed history, so the wiki must be a git \
                 working tree"
            )?;
            return Ok(2);
        }
    };

    // A bundle carries committed history only. Backing up a dirty tree would
    // produce an archive that silently lacks the newest work, so it is refused
    // and the pending paths are named. `serve --no-commit` users commit first.
    let status = match repo.status() {
        Ok(s) => s,
        Err(e) => {
            writeln!(out, "error: {e}")?;
            return Ok(2);
        }
    };
    if !status.entries.is_empty() {
        writeln!(
            out,
            "error: {} has uncommitted changes; a backup carries committed history only, so \
             this one would be missing them. Commit or discard first:",
            wiki.display()
        )?;
        for entry in &status.entries {
            writeln!(out, "  {entry}")?;
        }
        return Ok(2);
    }

    fs::create_dir_all(dest)?;
    let bundle = dest.join("wiki.bundle");
    if let Err(e) = repo.bundle_all(&bundle) {
        writeln!(out, "error: {e}")?;
        return Ok(2);
    }

    // Drafts and the operation journal are the state a clone does not carry.
    // The search index is deliberately absent: derived data is always
    // rebuildable, so archiving it would only make the backup stale faster.
    // The lock is absent because it describes a running process, not the wiki.
    let mut drafts = 0usize;
    let mut journal = false;
    if let Some(state) = store.appstate() {
        let drafts_src = state.drafts_dir();
        if drafts_src.is_dir() {
            copy_tree(&drafts_src, &dest.join("state/drafts"))?;
            drafts = fs::read_dir(&drafts_src)?.count();
        }
        let journal_src = state.journal_path();
        if journal_src.is_file() {
            fs::create_dir_all(dest.join("state"))?;
            fs::copy(&journal_src, dest.join("state/operation.journal"))?;
            journal = true;
        }
    }

    let head = repo.head().map(|h| h.0).unwrap_or_default();
    let branch = repo.current_branch().ok().flatten().unwrap_or_default();
    let origin = repo
        .origin_url()
        .ok()
        .flatten()
        .map(|u| strip_userinfo(&u))
        .unwrap_or_default();
    let repo_id = store
        .appstate()
        .map(|s| s.repo_id().to_string())
        .unwrap_or_default();
    // No timestamp: two backups of an unchanged wiki should be comparable, and
    // a clock reading would make every one of them differ for no information.
    let manifest = format!(
        "{BACKUP_MAGIC}\nrepo-id: {repo_id}\nhead: {head}\nbranch: {branch}\norigin: {origin}\n"
    );
    fs::write(dest.join("MANIFEST"), manifest)?;

    writeln!(out, "backed up {} to {}", wiki.display(), dest.display())?;
    writeln!(out, "  wiki.bundle          all refs and HEAD at {head}")?;
    writeln!(
        out,
        "  state/drafts         {drafts} draft{}",
        if drafts == 1 { "" } else { "s" }
    )?;
    writeln!(
        out,
        "  state/operation.journal  {}",
        if journal { "included" } else { "none yet" }
    )?;
    if origin.is_empty() {
        writeln!(out, "  origin               none recorded")?;
    } else {
        writeln!(out, "  origin               {origin}")?;
    }
    writeln!(
        out,
        "The search index is not archived: it is derived data and is rebuilt on demand."
    )?;
    Ok(0)
}

/// `berrywiki restore <backup-dir> <folder>`.
fn cmd_restore(args: &[String], out: &mut dyn Write) -> io::Result<i32> {
    let paths = positionals(args);
    let (Some(src), Some(dest)) = (paths.first(), paths.get(1)) else {
        writeln!(out, "usage: berrywiki restore <backup-dir> <folder>")?;
        return Ok(2);
    };
    let src = Path::new(src);
    let dest = Path::new(dest);

    let manifest_path = src.join("MANIFEST");
    let text = match fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => {
            writeln!(
                out,
                "error: {}: {e}; this does not look like a BerryWiki backup",
                manifest_path.display()
            )?;
            return Ok(2);
        }
    };
    if !text.starts_with(BACKUP_MAGIC) {
        writeln!(
            out,
            "error: {} does not begin with \"{BACKUP_MAGIC}\"; refusing to guess at the format",
            manifest_path.display()
        )?;
        return Ok(2);
    }
    let manifest = parse_manifest(&text);

    if !vacant_dir(dest)? {
        writeln!(
            out,
            "error: {} already has contents; restore will not write over an existing clone",
            dest.display()
        )?;
        return Ok(2);
    }

    let bundle = src.join("wiki.bundle");
    if !bundle.is_file() {
        writeln!(out, "error: {} is missing", bundle.display())?;
        return Ok(2);
    }
    let repo = match berrywiki_git::clone_from_bundle(&bundle, dest) {
        Ok(r) => r,
        Err(e) => {
            writeln!(out, "error: {e}")?;
            return Ok(2);
        }
    };

    // A clone taken from a bundle has an `origin` naming the bundle file. Left
    // alone it would be a remote that vanishes with the backup, so it is either
    // repointed at the URL the manifest recorded or dropped outright.
    let origin = manifest_value(&manifest, "origin").unwrap_or("");
    let origin_note = if origin.is_empty() {
        if let Err(e) = repo.forget_origin() {
            writeln!(out, "error: {e}")?;
            return Ok(2);
        }
        "no origin (none was recorded)".to_string()
    } else {
        if let Err(e) = repo.set_origin_url(origin) {
            writeln!(out, "error: {e}")?;
            return Ok(2);
        }
        format!("origin set to {origin}")
    };

    // App state is keyed by a hash of the wiki's path, so restoring to a new
    // directory means a new state dir. Writing the drafts back to the state of
    // the path they came *from* would leave them invisible here, which is the
    // trap this line exists to avoid.
    let mut drafts = 0usize;
    let mut journal = false;
    match berrywiki_appstate::AppState::for_wiki(dest) {
        Ok(state) => {
            let drafts_src = src.join("state/drafts");
            if drafts_src.is_dir() {
                copy_tree(&drafts_src, &state.drafts_dir())?;
                drafts = fs::read_dir(&drafts_src)?.count();
            }
            let journal_src = src.join("state/operation.journal");
            if journal_src.is_file() {
                if let Some(parent) = state.journal_path().parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&journal_src, state.journal_path())?;
                journal = true;
            }
        }
        Err(e) => {
            // The wiki itself is restored; only the side state is not. Say so
            // rather than failing the whole operation.
            writeln!(
                out,
                "warning: app state for {} could not be resolved ({e}); drafts and the \
                 operation journal were not restored",
                dest.display()
            )?;
        }
    }

    let head = repo.head().map(|h| h.0).unwrap_or_default();
    writeln!(out, "restored {} to {}", src.display(), dest.display())?;
    writeln!(out, "  HEAD                 {head}")?;
    writeln!(out, "  remote               {origin_note}")?;
    writeln!(
        out,
        "  drafts               {drafts} restored, journal {}",
        if journal { "restored" } else { "absent" }
    )?;
    if let Some(recorded) = manifest_value(&manifest, "head") {
        if !recorded.is_empty() && recorded != head {
            writeln!(
                out,
                "warning: manifest recorded HEAD {recorded}, which is not what the bundle \
                 restored; the backup may be inconsistent"
            )?;
        }
    }
    Ok(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn fixture() -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/test-wiki")
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn run_to_string(args: &[&str]) -> (i32, String) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut buf = Vec::new();
        let code = run(&args, &mut buf).unwrap();
        (code, String::from_utf8(buf).unwrap())
    }

    #[test]
    fn check_fixture_is_clean_exit_zero() {
        let (code, out) = run_to_string(&["check", &fixture()]);
        assert_eq!(code, 0, "fixture has only warnings, not errors:\n{out}");
        assert!(out.contains("10 page(s)"));
        assert!(out.contains("- Home"));
        assert!(out.contains("link.broken"), "broken link reported");
        assert!(out.contains("warning(s)"));
    }

    #[test]
    fn check_reports_errors_with_exit_one() {
        let dir = std::env::temp_dir().join(format!(
            "berrywiki-cli-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        let dup = "<!-- berrywiki\nid: same-id\nparent: null\nposition: 0\nkind: page\ntags: []\narchived: false\n-->\n\n# One\n";
        fs::write(dir.join("One.md"), dup).unwrap();
        fs::write(dir.join("Two.md"), dup.replace("# One", "# Two")).unwrap();

        let (code, out) = run_to_string(&["check", dir.to_str().unwrap()]);
        assert_eq!(code, 1, "duplicate id is an error → exit 1");
        assert!(out.contains("graph.duplicate-id"));
        assert!(out.contains("1 error(s)") || out.contains("error(s)"));
    }

    #[test]
    fn sidebar_write_is_refused_while_another_writer_holds_the_lock() {
        let dir = std::env::temp_dir().join(format!(
            "berrywiki-cli-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Home.md"),
            "<!-- berrywiki\nid: home\nparent: null\nposition: 0\nkind: page\ntags: []\narchived: false\n-->\n\n# Home\n",
        )
        .unwrap();
        let path = dir.to_str().unwrap();

        let (code, out) = run_to_string(&["sidebar", path, "--write"]);
        assert_eq!(code, 0, "{out}");

        let state = berrywiki_appstate::AppState::for_wiki(&dir).unwrap();
        let holder = RepoLock::acquire(&state, "test-holder").unwrap();
        let (code, out) = run_to_string(&["sidebar", path, "--write"]);
        assert_eq!(code, 2, "a held lock refuses the write: {out}");
        assert!(out.contains("already in use"), "{out}");
        assert!(out.contains("program=test-holder"), "{out}");
        assert!(out.contains("only one writer"), "{out}");
        drop(holder);

        let (code, _) = run_to_string(&["sidebar", path, "--write"]);
        assert_eq!(code, 0, "released lock admits the write again");
    }
    #[test]
    fn check_missing_folder_exits_two() {
        let (code, out) = run_to_string(&["check", "/no/such/wiki"]);
        assert_eq!(code, 2);
        assert!(out.contains("error:"));
    }

    #[test]
    fn sidebar_prints_generated_form() {
        let (code, out) = run_to_string(&["sidebar", &fixture()]);
        assert_eq!(code, 0);
        assert!(out.starts_with("# Notebook"));
        assert!(out.contains("[Home](Home)"));
        assert!(!out.contains("Archived Old Page"), "archived excluded");
    }

    #[test]
    fn help_and_unknown() {
        let (code, out) = run_to_string(&["--help"]);
        assert_eq!(code, 0);
        assert!(out.contains("USAGE:"));

        // Every subcommand the dispatcher answers must be discoverable, or the
        // command exists only for whoever read the source.
        for cmd in ["check", "sidebar", "serve", "backup", "restore"] {
            assert!(out.contains(cmd), "{cmd} is missing from USAGE");
        }

        let (code, out) = run_to_string(&["frobnicate"]);
        assert_eq!(code, 2);
        assert!(out.contains("unknown command"));
    }

    #[test]
    fn credentials_never_reach_the_manifest() {
        // A backup directory gets copied to other machines, so an origin URL
        // with an embedded password would turn "keep a copy" into "publish the
        // password".
        assert_eq!(
            strip_userinfo("https://user:s3cret@github.com/o/r.wiki.git"),
            "https://github.com/o/r.wiki.git"
        );
        assert_eq!(
            strip_userinfo("https://token@github.com/o/r.wiki.git"),
            "https://github.com/o/r.wiki.git"
        );
        // An `@` after the authority belongs to the path, not to a credential.
        assert_eq!(
            strip_userinfo("https://github.com/o/r@v1.git"),
            "https://github.com/o/r@v1.git"
        );
        // scp-style syntax has no password field; the `@` is the user.
        assert_eq!(
            strip_userinfo("git@github.com:o/r.wiki.git"),
            "git@github.com:o/r.wiki.git"
        );
        assert_eq!(strip_userinfo("/srv/wikis/r.git"), "/srv/wikis/r.git");
    }
}
