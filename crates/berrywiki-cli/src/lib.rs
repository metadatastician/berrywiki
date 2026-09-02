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

use std::io::{self, Write};

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

fn has_flag(args: &[String], flag: &str) -> bool {
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
enum WriterLock {
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
fn writer_lock(
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

        let (code, out) = run_to_string(&["frobnicate"]);
        assert_eq!(code, 2);
        assert!(out.contains("unknown command"));
    }
}
