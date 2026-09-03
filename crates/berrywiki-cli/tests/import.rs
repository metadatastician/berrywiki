// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! `berrywiki import` against a real git working tree.
//!
//! An import is the one command that writes many pages at once, so the tests
//! that matter most are the ones proving it *doesn't* write: the dry run, the
//! three refusals, and the second run that finds its own earlier work and
//! leaves it alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;

use berrywiki_git_compat::GitSandbox;

// App state is keyed by a hash of the wiki's path, so one temp XDG_STATE_HOME
// shared across tests stays isolated per sandbox. Set once, before any store
// opens, so threaded tests never race on the env var.
static XDG: Once = Once::new();
fn init_xdg() {
    XDG.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bw-cli-import-xdg-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("XDG_STATE_HOME", &dir);
    });
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-wiki")
        .canonicalize()
        .expect("fixture exists")
}

fn run(args: &[&str]) -> (i32, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut buf = Vec::new();
    let code = berrywiki_cli::run(&args, &mut buf).expect("cli ran");
    (code, String::from_utf8(buf).expect("utf-8 output"))
}

/// Three pages, one of them a child, with an accented character so the
/// character-offset path is exercised end to end rather than only in the
/// parser's own unit tests.
const NOTEBOOK: &str = r#"<cherrytree>
<node name="Recipes" unique_id="1"><rich_text>Things to cook.</rich_text>
<node name="Cafe cr&#232;me" unique_id="2"><rich_text>Coffee, milk.</rich_text></node>
</node>
<node name="Shopping" unique_id="3"><rich_text>Beans.</rich_text></node>
</cherrytree>
"#;

/// Write a notebook somewhere under the sandbox and return its path.
fn notebook_at(sb: &GitSandbox, rel: &str, body: &str) -> PathBuf {
    let path = sb.root.join(rel);
    fs::create_dir_all(path.parent().expect("has a parent")).expect("made the directory");
    fs::write(&path, body).expect("wrote the notebook");
    path
}

fn commit_count(sb: &GitSandbox, repo: &Path) -> usize {
    sb.git(repo, &["rev-list", "--count", "HEAD"])
        .expect_success("rev-list --count")
        .stdout
        .trim()
        .parse()
        .expect("a number")
}

/// Every Markdown file in the wiki that carries an import marker.
fn imported_pages(wiki: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(wiki)
        .expect("read the wiki")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter(|p| fs::read_to_string(p).is_ok_and(|s| s.contains("source: ")))
        .collect();
    found.sort();
    found
}

// ---------- dry run ----------

#[test]
fn a_dry_run_reports_what_it_would_do_and_writes_nothing() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "notes.ctd", NOTEBOOK);

    let before = commit_count(&sb, &sb.ours);
    let (code, out) = run(&["import", nb.to_str().unwrap(), sb.ours.to_str().unwrap()]);

    assert_eq!(code, 0, "a readable notebook is not an error:\n{out}");
    assert!(
        out.contains("dry-run"),
        "the report says it wrote nothing:\n{out}"
    );
    assert!(
        out.contains("3 to create"),
        "the plan counts every node:\n{out}"
    );
    assert!(
        imported_pages(&sb.ours).is_empty(),
        "a dry run wrote pages into the wiki"
    );
    assert_eq!(
        commit_count(&sb, &sb.ours),
        before,
        "a dry run made a commit"
    );
}

#[test]
fn the_report_names_the_notebook_and_never_the_directory_holding_it() {
    // A report is the thing a user pastes into a bug tracker. The path above
    // a personal notebook is nobody else's business, so only the file's own
    // name may appear.
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "secret-dir/notes.ctd", NOTEBOOK);

    let (code, out) = run(&["import", nb.to_str().unwrap(), sb.ours.to_str().unwrap()]);

    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("notes.ctd"),
        "the report names the file:\n{out}"
    );
    assert!(
        !out.contains("secret-dir"),
        "the report leaked the directory holding the notebook:\n{out}"
    );
}

#[test]
fn the_json_report_is_machine_readable_and_equally_private() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "secret-dir/notes.ctd", NOTEBOOK);

    let (code, out) = run(&[
        "import",
        nb.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
        "--json",
    ]);

    assert_eq!(code, 0, "{out}");
    assert!(
        out.trim_start().starts_with('{'),
        "JSON was asked for:\n{out}"
    );
    assert!(
        !out.contains("secret-dir"),
        "the JSON leaked the path:\n{out}"
    );
}

// ---------- applying ----------

#[test]
fn applying_writes_every_page_in_exactly_one_commit() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "notes.ctd", NOTEBOOK);

    let before = commit_count(&sb, &sb.ours);
    let (code, out) = run(&[
        "import",
        nb.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
        "--apply",
    ]);

    assert_eq!(code, 0, "the import failed:\n{out}");
    assert_eq!(
        imported_pages(&sb.ours).len(),
        3,
        "every node became a page:\n{out}"
    );
    assert_eq!(
        commit_count(&sb, &sb.ours) - before,
        1,
        "an import is one act and must be one commit:\n{out}"
    );
    assert!(
        sb.git(&sb.ours, &["status", "--porcelain"])
            .expect_success("status")
            .stdout
            .trim()
            .is_empty(),
        "the import left something uncommitted"
    );
    // The sidebar is part of the same commit, not a follow-up.
    let touched = sb
        .git(&sb.ours, &["show", "--name-only", "--format=", "HEAD"])
        .expect_success("show")
        .stdout;
    assert!(
        touched.contains("_Sidebar.md"),
        "the sidebar was not in the import commit:\n{touched}"
    );
    assert!(
        out.contains("berrywiki sync") || out.contains("git push"),
        "the report should say how to publish:\n{out}"
    );
}

#[test]
fn the_child_node_becomes_a_child_page() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "notes.ctd", NOTEBOOK);
    let (code, out) = run(&[
        "import",
        nb.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
        "--apply",
    ]);
    assert_eq!(code, 0, "{out}");

    let bodies: Vec<String> = imported_pages(&sb.ours)
        .iter()
        .map(|p| fs::read_to_string(p).expect("readable"))
        .collect();
    let recipes = bodies
        .iter()
        .find(|b| b.contains("# Recipes"))
        .expect("the parent page exists");
    let recipes_id = recipes
        .lines()
        .find_map(|l| l.trim().strip_prefix("id: "))
        .expect("the parent has an id")
        .to_string();
    assert!(
        bodies
            .iter()
            .any(|b| b.contains(&format!("parent: {recipes_id}"))),
        "the nested node did not become a child page"
    );
}

#[test]
fn importing_the_same_notebook_twice_writes_nothing_the_second_time() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "notes.ctd", NOTEBOOK);
    let args = [
        "import",
        nb.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
        "--apply",
    ];

    let (code, out) = run(&args);
    assert_eq!(code, 0, "{out}");
    let after_first = commit_count(&sb, &sb.ours);

    let (code, out) = run(&args);
    assert_eq!(code, 0, "a second import is a no-op, not an error:\n{out}");
    assert_eq!(
        imported_pages(&sb.ours).len(),
        3,
        "the second run duplicated pages:\n{out}"
    );
    assert_eq!(
        commit_count(&sb, &sb.ours),
        after_first,
        "the second run committed something:\n{out}"
    );
    // The report must agree with the commit log. Saying "wrote 0 pages in one
    // commit" when no commit was made is the kind of contradiction that
    // teaches a user to distrust every other line of the report.
    assert!(
        out.contains("had already been imported"),
        "the second run does not say why it wrote nothing:\n{out}"
    );
    assert!(
        !out.contains("in one commit"),
        "the second run claims a commit it did not make:\n{out}"
    );
}

#[test]
fn an_edited_page_survives_a_second_import_untouched() {
    // Idempotence must not become "re-import overwrites your edits". A page
    // that carries this node's marker is ours to recognise, not to rewrite.
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "notes.ctd", NOTEBOOK);
    let args = [
        "import",
        nb.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
        "--apply",
    ];
    let (code, out) = run(&args);
    assert_eq!(code, 0, "{out}");

    let page = imported_pages(&sb.ours)
        .into_iter()
        .find(|p| {
            fs::read_to_string(p)
                .expect("readable")
                .contains("# Shopping")
        })
        .expect("the page exists");
    let edited = fs::read_to_string(&page)
        .expect("readable")
        .replace("Beans.", "Beans, and coffee I chose myself.");
    fs::write(&page, &edited).expect("edited the page");

    let (code, out) = run(&args);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        fs::read_to_string(&page).expect("readable"),
        edited,
        "a second import overwrote the user's own edit"
    );
}

// ---------- the refusals ----------

#[test]
fn a_page_that_is_not_ours_stops_the_run_before_any_write() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "notes.ctd", NOTEBOOK);
    let args = [
        "import",
        nb.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
        "--apply",
    ];
    let (code, out) = run(&args);
    assert_eq!(code, 0, "{out}");

    // Somebody rewrote one page's provenance. It is now their page, not the
    // importer's, and re-importing must not reclaim it.
    let page = imported_pages(&sb.ours)
        .into_iter()
        .next()
        .expect("a page exists");
    let text = fs::read_to_string(&page).expect("readable");
    let line = text
        .lines()
        .find(|l| l.starts_with("source: "))
        .expect("the marker line")
        .to_string();
    fs::write(
        &page,
        text.replace(&line, "source: somebody-elses-notebook#1"),
    )
    .expect("rewrote the marker");
    sb.git(&sb.ours, &["commit", "-am", "claim the page"])
        .expect_success("commit");

    let before = commit_count(&sb, &sb.ours);
    let (code, out) = run(&args);
    assert_eq!(code, 2, "the run should have been refused:\n{out}");
    assert!(
        out.contains("occupied"),
        "the refusal names the page:\n{out}"
    );
    assert!(
        out.contains("nothing has been changed"),
        "the refusal should say nothing was written:\n{out}"
    );
    assert_eq!(
        commit_count(&sb, &sb.ours),
        before,
        "the refused run committed something"
    );
}

#[test]
fn siblings_that_share_a_title_stop_an_apply() {
    // Filenames encode ancestry (ADR-0001), so the second write would land on
    // a disambiguated name nobody chose. Refusing is the whole point.
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(
        &sb,
        "notes.ctd",
        r#"<cherrytree>
<node name="Notes" unique_id="1"><rich_text>a</rich_text></node>
<node name="Notes" unique_id="2"><rich_text>b</rich_text></node>
</cherrytree>
"#,
    );

    let (code, out) = run(&[
        "import",
        nb.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
        "--apply",
    ]);

    assert_eq!(code, 2, "colliding titles should be refused:\n{out}");
    assert!(out.contains("Notes"), "the refusal names the title:\n{out}");
    assert!(
        imported_pages(&sb.ours).is_empty(),
        "the refused run wrote pages anyway"
    );
}

#[test]
fn a_folder_that_is_not_a_git_working_tree_is_refused_with_the_cure() {
    // Without a commit there is no one-command undo for an N-page write, so
    // the import declines rather than leaving the user to clean up by hand.
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "notes.ctd", NOTEBOOK);
    let plain = sb.root.join("not-a-repo");
    fs::create_dir_all(&plain).expect("made the folder");

    let (code, out) = run(&[
        "import",
        nb.to_str().unwrap(),
        plain.to_str().unwrap(),
        "--apply",
    ]);

    assert_eq!(code, 2, "a non-git folder should be refused:\n{out}");
    assert!(
        out.contains("git init"),
        "the refusal names the cure:\n{out}"
    );
    assert!(
        imported_pages(&plain).is_empty(),
        "the refused run wrote pages anyway"
    );
}

#[test]
fn a_dry_run_into_a_plain_folder_still_works() {
    // The git requirement is about writing, not about reading. Someone
    // deciding whether to import should not need a repository first.
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let nb = notebook_at(&sb, "notes.ctd", NOTEBOOK);
    let plain = sb.root.join("plain-preview");
    fs::create_dir_all(&plain).expect("made the folder");

    let (code, out) = run(&["import", nb.to_str().unwrap(), plain.to_str().unwrap()]);
    assert_eq!(code, 0, "a dry run needs no repository:\n{out}");
    assert!(out.contains("3 to create"), "{out}");
}

#[test]
fn a_password_protected_notebook_is_refused_with_advice_not_a_parse_error() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    // 7-zip magic: what a .ctz or .ctx actually is on disk.
    let nb = sb.root.join("locked.ctz");
    fs::write(&nb, [0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0x00, 0x04]).expect("wrote it");

    let (code, out) = run(&[
        "import",
        nb.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
        "--apply",
    ]);

    assert_eq!(code, 2, "{out}");
    assert!(
        out.to_lowercase().contains("cherrytree"),
        "the refusal should tell the user what to do in CherryTree:\n{out}"
    );
    assert!(imported_pages(&sb.ours).is_empty());
}

#[test]
fn a_sqlite_notebook_is_refused_by_content_not_by_its_name() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    // Named .ctd, but the bytes say SQLite. Content wins.
    let nb = sb.root.join("mislabelled.ctd");
    let mut bytes = b"SQLite format 3\0".to_vec();
    bytes.extend_from_slice(&[0u8; 16]);
    fs::write(&nb, bytes).expect("wrote it");

    let (code, out) = run(&["import", nb.to_str().unwrap(), sb.ours.to_str().unwrap()]);
    assert_eq!(code, 2, "{out}");
    assert!(imported_pages(&sb.ours).is_empty());
}

#[test]
fn a_missing_notebook_is_an_error_not_a_panic() {
    init_xdg();
    let sb = GitSandbox::create(&fixture_dir());
    let missing = sb.root.join("no-such-file.ctd");
    let (code, out) = run(&[
        "import",
        missing.to_str().unwrap(),
        sb.ours.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("error"), "{out}");
}

#[test]
fn the_wrong_number_of_arguments_prints_the_usage() {
    init_xdg();
    let (code, out) = run(&["import"]);
    assert_eq!(code, 2);
    assert!(out.contains("usage: berrywiki import"), "{out}");
}
