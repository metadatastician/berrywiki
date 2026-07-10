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

use berrywiki_core::{generate_sidebar, Severity, SidebarOptions};
use berrywiki_store::{LocalFolderStore, WikiStore};

const USAGE: &str = "\
berrywiki — inspect and maintain a wiki folder

USAGE:
    berrywiki check <folder>
    berrywiki sidebar <folder> [--write]
    berrywiki --help

COMMANDS:
    check      Load the wiki and print its tree + diagnostics. Exit code 1 if
               any error-level diagnostic is found, else 0.
    sidebar    Print the generated _Sidebar.md, or regenerate it with --write.
";

/// Run the CLI. Returns the process exit code. All output (including error
/// messages) goes to `out`; nothing is printed to real stdout/stderr here.
pub fn run(args: &[String], out: &mut dyn Write) -> io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("check") => cmd_check(first_path(&args[1..]), out),
        Some("sidebar") => cmd_sidebar(first_path(&args[1..]), has_flag(&args[1..], "--write"), out),
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
    args.iter().find(|a| !a.starts_with("--")).map(String::as_str)
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
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
        let marker = if page.is_archived() { " (archived)" } else { "" };
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
