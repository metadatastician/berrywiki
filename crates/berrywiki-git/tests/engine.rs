//! Behavioural tests for the git engine, run against a real bare remote plus
//! two clones ([`berrywiki_git_compat::GitSandbox`]). Every test that mutates
//! also asserts that nothing was lost — the property the engine exists to hold.

use std::fs;
use std::path::PathBuf;

use berrywiki_git::{GitRepo, Identity, IntegrateOutcome, PushOutcome};
use berrywiki_git_compat::GitSandbox;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/test-wiki")
        .canonicalize()
        .expect("fixture exists")
}

// ---------- open / inspect ----------

#[test]
fn open_rejects_a_non_repo() {
    let tmp = std::env::temp_dir().join(format!("berrywiki-git-nonrepo-{}", std::process::id()));
    fs::create_dir_all(&tmp).unwrap();
    assert!(GitRepo::open(&tmp).is_err(), "a plain directory is not a repo");
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn open_and_status_on_a_clean_clone() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open clone");
    assert!(repo.is_clean().unwrap(), "freshly seeded clone is clean");
    assert_eq!(repo.head().unwrap().0, sb.head(&sb.ours), "head matches git");
}

// ---------- commit ----------

#[test]
fn commit_all_commits_pending_changes_then_reports_nothing_to_do() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open");

    fs::write(sb.ours.join("Research.md"), "# Research\n\nlocal edit\n").unwrap();
    assert!(!repo.is_clean().unwrap(), "edit is pending");

    let id = repo
        .commit_all("Edit research page")
        .expect("commit ok")
        .expect("something was committed");
    assert_eq!(id, repo.head().unwrap(), "returned id is the new HEAD");
    assert!(repo.is_clean().unwrap(), "tree clean after commit");

    // Second call has nothing to do — a no-op, never an empty commit.
    let again = repo.commit_all("Nothing changed").expect("commit ok");
    assert!(again.is_none(), "no commit when there is nothing to commit");
}

#[test]
fn commit_all_records_the_configured_identity() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open").with_identity(Identity {
        name: "Ada Lovelace".to_string(),
        email: "ada@example.invalid".to_string(),
    });
    fs::write(sb.ours.join("Research.md"), "# Research\n\nby ada\n").unwrap();
    repo.commit_all("Ada edits").unwrap().unwrap();

    let author = sb
        .git(&sb.ours, &["log", "-1", "--format=%an <%ae>"])
        .expect_success("log")
        .stdout
        .trim()
        .to_string();
    assert_eq!(author, "Ada Lovelace <ada@example.invalid>");
}

// ---------- fetch / divergence ----------

#[test]
fn fetch_then_divergence_sees_the_remote_advance() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open");

    // They push a commit; before we fetch we cannot see it.
    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");

    repo.fetch().expect("fetch");
    let div = repo.divergence().expect("divergence");
    assert!(div.has_upstream);
    assert_eq!(div.behind, 1, "one remote commit to integrate");
    assert_eq!(div.ahead, 0);
    assert!(div.can_fast_forward(), "clean fast-forward available");
    assert!(!div.needs_merge());
}

#[test]
fn divergence_reports_both_sides_when_histories_diverge() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open");

    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");
    sb.commit_change(&sb.ours, "Teaching.md", "# Teaching\n\nours\n", "Ours");

    repo.fetch().expect("fetch");
    let div = repo.divergence().expect("divergence");
    assert_eq!((div.ahead, div.behind), (1, 1));
    assert!(div.needs_merge(), "diverged histories need a real merge");
    assert!(!div.can_fast_forward());
}

// ---------- fast-forward integration ----------

#[test]
fn fast_forward_advances_local_to_a_purely_remote_change() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open");

    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");

    repo.fetch().expect("fetch");
    assert_eq!(
        repo.fast_forward_to_upstream().unwrap(),
        IntegrateOutcome::FastForwarded
    );
    let content = fs::read_to_string(sb.ours.join("Research.md")).unwrap();
    assert!(content.contains("theirs"), "remote content now local");
    assert!(repo.divergence().unwrap().is_up_to_date());

    // Nothing further to integrate.
    assert_eq!(
        repo.fast_forward_to_upstream().unwrap(),
        IntegrateOutcome::AlreadyUpToDate
    );
}

#[test]
fn fast_forward_refuses_to_touch_a_diverged_history() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open");

    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");
    sb.commit_change(&sb.ours, "Teaching.md", "# Teaching\n\nours\n", "Ours");
    let our_head = sb.head(&sb.ours);

    repo.fetch().expect("fetch");
    assert_eq!(
        repo.fast_forward_to_upstream().unwrap(),
        IntegrateOutcome::NeedsManualMerge,
        "diverged history is not fast-forwardable"
    );
    // Local was left exactly as it was — no merge commit, no rewind.
    assert_eq!(sb.head(&sb.ours), our_head, "HEAD untouched");
    assert!(fs::read_to_string(sb.ours.join("Teaching.md")).unwrap().contains("ours"));
    assert!(repo.is_clean().unwrap(), "no half-finished merge left behind");
}

// ---------- push ----------

#[test]
fn push_publishes_a_fast_forward_then_is_up_to_date() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open");

    fs::write(sb.ours.join("Teaching.md"), "# Teaching\n\nours\n").unwrap();
    repo.commit_all("Local edit").unwrap().unwrap();

    assert_eq!(repo.push().unwrap(), PushOutcome::Pushed);
    assert_eq!(sb.behind_by(&sb.theirs), 1, "remote advanced for the other clone");

    // Re-pushing with no new commits is a clean no-op.
    assert_eq!(repo.push().unwrap(), PushOutcome::UpToDate);
}

/// The keystone test: when the remote has moved on, a push is *rejected*, and
/// both the local commit and the remote tip are left exactly as they were. This
/// is what "force-push is unrepresentable" means in observable behaviour.
#[test]
fn push_is_rejected_when_remote_moved_and_nothing_is_overwritten() {
    let sb = GitSandbox::create(&fixture_dir());
    let repo = GitRepo::open(&sb.ours).expect("open");

    // They publish first.
    sb.commit_change(&sb.theirs, "Research.md", "# Research\n\ntheirs\n", "Theirs");
    sb.git(&sb.theirs, &["push", "origin", "main"]).expect_success("their push");
    let their_head = sb.head(&sb.theirs);
    let remote_before = sb.head(&sb.remote);
    assert_eq!(remote_before, their_head, "remote is at their commit");

    // We commit a different file without integrating.
    fs::write(sb.ours.join("Teaching.md"), "# Teaching\n\nours\n").unwrap();
    let our_head = repo.commit_all("Ours").unwrap().unwrap();

    // The push must be refused — never forced.
    assert_eq!(repo.push().unwrap(), PushOutcome::RejectedNonFastForward);

    // Local commit intact...
    assert_eq!(repo.head().unwrap(), our_head, "our commit survives");
    assert!(fs::read_to_string(sb.ours.join("Teaching.md")).unwrap().contains("ours"));
    // ...and the remote was not overwritten.
    assert_eq!(sb.head(&sb.remote), their_head, "remote tip unchanged — not forced");
}

#[test]
fn push_reports_no_upstream_when_the_branch_has_none() {
    let sb = GitSandbox::create(&fixture_dir());
    // Detach the branch from its upstream to model an un-tracked branch.
    sb.git(&sb.ours, &["branch", "--unset-upstream"]).expect_success("unset upstream");
    let repo = GitRepo::open(&sb.ours).expect("open");

    assert!(!repo.divergence().unwrap().has_upstream);
    assert_eq!(repo.push().unwrap(), PushOutcome::NoUpstream);
    assert_eq!(
        repo.fast_forward_to_upstream().unwrap(),
        IntegrateOutcome::NoUpstream
    );
}

/// A server-side decline ("[remote rejected]": a hook, a protected branch,
/// push-protection) is NOT a non-fast-forward: it must reach the caller as an
/// error carrying the diagnostic, not be masked as the transient
/// fetch-and-retry outcome (a review finding — the bare "rejected" substring
/// used to swallow it).
#[cfg(unix)]
#[test]
fn push_surfaces_a_server_side_decline_as_an_error() {
    use berrywiki_git::GitError;
    use std::os::unix::fs::PermissionsExt;

    let sb = GitSandbox::create(&fixture_dir());
    // A pre-receive hook on the bare remote that declines every push.
    let hook = sb.remote.join("hooks/pre-receive");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();

    let repo = GitRepo::open(&sb.ours).expect("open");
    fs::write(sb.ours.join("Teaching.md"), "# Teaching\n\nours\n").unwrap();
    let our_head = repo.commit_all("Local edit").unwrap().unwrap();

    match repo.push() {
        Err(GitError::Git { op, stderr }) => {
            assert_eq!(op, "push");
            let s = stderr.to_lowercase();
            assert!(
                s.contains("rejected") || s.contains("hook"),
                "the real diagnostic reaches the caller: {stderr}"
            );
        }
        other => panic!("hook decline must surface as an error, not {other:?}"),
    }
    // The failed push left the local commit exactly as it was.
    assert_eq!(repo.head().unwrap(), our_head, "local commit intact after decline");
}

/// A staged rename is ONE logical change; porcelain `-z` encodes it as two
/// NUL-separated fields, which naive splitting would report as two entries with
/// a prefix-less phantom (a review finding). status() must fold it back.
#[test]
fn status_folds_a_staged_rename_into_one_entry() {
    let sb = GitSandbox::create(&fixture_dir());
    sb.git(&sb.ours, &["mv", "Research.md", "Research-Renamed.md"]).expect_success("git mv");
    let repo = GitRepo::open(&sb.ours).expect("open");

    let st = repo.status().unwrap();
    assert!(!st.is_clean());
    assert_eq!(st.entries.len(), 1, "a rename is one change, not two: {:?}", st.entries);
    let entry = &st.entries[0];
    assert!(entry.starts_with('R'), "keeps the rename status code: {entry}");
    assert!(
        entry.contains("Research.md") && entry.contains("Research-Renamed.md"),
        "shows both the original and the new path: {entry}"
    );
    assert!(
        !st.entries.iter().any(|x| x == "Research.md"),
        "the original path is not emitted as its own prefix-less entry: {:?}",
        st.entries
    );
}
