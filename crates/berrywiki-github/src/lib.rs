// SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
// SPDX-License-Identifier: MPL-2.0

//! GitHub Wiki read adapter.
//!
//! A GitHub wiki is a separate `<repo>.wiki.git` git repository — there is no
//! page API. This adapter maintains a clean local *mirror* of that repo and
//! exposes it through [`berrywiki_store::LocalFolderStore`], so the whole
//! read-only stack (engine, SSR explorer, CLI) works over a real GitHub wiki
//! without any GitHub-specific code above this layer.
//!
//! # Scope
//!
//! Read-only (Phase 1): clone/update + read. It is a *managed mirror* — it does
//! not hold local edits and will hard-reset the cache to the remote on
//! [`GitHubWiki::pull`]. In-app editing and push are Phase 2/3, with a
//! different (never-discard-local-work) sync strategy.
//!
//! # Read-only fence (P3-serve-sync)
//!
//! This crate is the **mirror** path and only the mirror path. It is wired
//! to `berrywiki serve --github`, which is read-only by construction
//! (`serve_readonly`), and to `check`/`sidebar` over a mirror. The
//! hard-reset in [`GitHubWiki::pull`] is safe *only* because nothing in this
//! crate ever holds a local edit. Editing against a real wiki goes through
//! `berrywiki-sync` over an ordinary clone (commit-on-save, fetch, fast-forward
//! or hand off, never reset, never force). Do not add write paths here and do
//! not point the editor at a mirror.
//!
//! # Credentials & honesty
//!
//! Public wikis clone anonymously. Private wikis authenticate with a token via
//! `GIT_ASKPASS` (never embedded in a logged URL; redacted from any error).
//! The token is confined: it is offered only to `https://github.com/` remotes
//! ([`token_permitted_for`]), git never follows an HTTP redirect while it is in
//! play, and the helper lives in a freshly created owner-only directory. The
//! cache is reset only if it carries this crate's marker for the same remote.
//! The
//! token path is **UNVERIFIED against live GitHub** — no real wiki has been
//! exercised here (no `gh`/token on the dev host). Per ADR-0002 this is stated,
//! not faked: the git mechanics are tested against a local bare repo; the
//! URL/redaction/askpass logic is unit-tested; the live round-trip is a
//! credential-gated spike (work package P1-spike-read).

use std::path::{Path, PathBuf};
use std::process::Command;

use berrywiki_store::{LocalFolderStore, StoreError, WikiStore};

pub type Result<T> = std::result::Result<T, GithubError>;

#[derive(Debug)]
pub enum GithubError {
    /// `repo` could not be interpreted as a wiki target.
    BadRepo(String),
    /// A token was supplied for a remote it must never be sent to.
    TokenRefused(String),
    /// The cache directory exists but is not a mirror this crate created for
    /// this remote, so it will not be reset.
    NotOurMirror(String),
    /// A `git` invocation failed. `stderr` has the token redacted.
    Git {
        context: String,
        stderr: String,
    },
    Store(StoreError),
    Io(std::io::Error),
}

impl std::fmt::Display for GithubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GithubError::BadRepo(r) => write!(f, "Not a usable wiki target: {r:?}."),
            GithubError::TokenRefused(r) => write!(
                f,
                "Refusing to send a token to {r:?}: tokens go only to https://github.com/. \
                 Use an https://github.com/ remote, or drop the token for a public or local remote."
            ),
            GithubError::NotOurMirror(why) => write!(
                f,
                "Refusing to update the cache: {why}. It may hold someone's work, so it is not reset. \
                 Point --cache at an empty or BerryWiki-created directory (a mirror is regenerable)."
            ),
            GithubError::Git { context, stderr } => {
                write!(f, "{context} failed: {}", stderr.trim())
            }
            GithubError::Store(e) => write!(f, "{e}"),
            GithubError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for GithubError {}
impl From<StoreError> for GithubError {
    fn from(e: StoreError) -> Self {
        GithubError::Store(e)
    }
}
impl From<std::io::Error> for GithubError {
    fn from(e: std::io::Error) -> Self {
        GithubError::Io(e)
    }
}

/// Resolve a wiki git URL from `owner/name`, a GitHub repo URL, a full
/// `.wiki.git` URL, or a direct git remote (e.g. a local bare repo path).
pub fn wiki_git_url(repo: &str) -> Result<String> {
    let r = repo.trim();
    if r.is_empty() {
        return Err(GithubError::BadRepo(repo.to_string()));
    }
    if r.ends_with(".wiki.git") {
        return Ok(r.to_string());
    }
    let is_github = r.contains("github.com");
    if r.starts_with("http://") || r.starts_with("https://") || r.starts_with("git@") {
        if is_github {
            let base = r.strip_suffix(".git").unwrap_or(r);
            return Ok(format!("{base}.wiki.git"));
        }
        // Some other git host: pass the remote through untouched.
        return Ok(r.to_string());
    }
    // `owner/name` shorthand → github wiki URL. Exclude filesystem paths
    // (absolute, `./`, `~`) and `.git` remotes so a two-segment local bare-repo
    // path like `/tmp/x.git` is not misread as a GitHub slug.
    let looks_like_path = r.starts_with('/') || r.starts_with('.') || r.starts_with('~');
    let segs: Vec<&str> = r.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() == 2
        && !looks_like_path
        && !r.ends_with(".git")
        && !r.contains(':')
        && !segs[0].contains('.')
    {
        return Ok(format!(
            "https://github.com/{}/{}.wiki.git",
            segs[0], segs[1]
        ));
    }
    // Otherwise treat as a direct remote (local bare repo, other URL form).
    Ok(r.to_string())
}

/// Whether a token may be sent to `remote`: only plain `https://github.com/`
/// URLs, with no credentials or port smuggled into the authority, ever receive
/// it. Anything else (another host, `http://`, SSH, a local path) is refused.
pub fn token_permitted_for(remote: &str) -> bool {
    let Some(rest) = remote.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    authority.eq_ignore_ascii_case("github.com") && rest.len() > authority.len() + 1
}

/// Name of the marker this crate writes inside a mirror's `.git` directory.
/// Inside `.git` so it never appears in the wiki's tree.
const MIRROR_MARKER: &str = "berrywiki-mirror";

/// Redact a token from arbitrary text (for safe error surfacing/logging).
pub fn redact_token(text: &str, token: Option<&str>) -> String {
    match token {
        Some(t) if !t.is_empty() => text.replace(t, "***"),
        _ => text.to_string(),
    }
}

/// The `GIT_ASKPASS` helper script body. It answers the *username* prompt with
/// `x-access-token` and the *password* prompt with the token — distinguishing
/// the two so the token is not echoed as the username.
pub fn askpass_script() -> &'static str {
    "#!/bin/sh\n\
case \"$1\" in\n\
*[Uu]sername*) printf '%s' \"${GIT_USERNAME:-x-access-token}\" ;;\n\
*) printf '%s' \"$BERRYWIKI_TOKEN\" ;;\n\
esac\n"
}

/// A maintained local mirror of a GitHub wiki, read through a `LocalFolderStore`.
pub struct GitHubWiki {
    remote: String,
    dest: PathBuf,
    token: Option<String>,
    store: LocalFolderStore,
}

impl GitHubWiki {
    /// Clone (or update an existing) mirror at `dest` and open it read-only.
    pub fn open(repo: &str, dest: impl Into<PathBuf>, token: Option<&str>) -> Result<Self> {
        let remote = wiki_git_url(repo)?;
        if token.is_some() && !token_permitted_for(&remote) {
            return Err(GithubError::TokenRefused(remote));
        }
        let dest = dest.into();
        clone_or_update(&remote, &dest, token)?;
        let store = LocalFolderStore::open(&dest)?;
        Ok(GitHubWiki {
            remote,
            dest,
            token: token.map(String::from),
            store,
        })
    }

    /// The read-only store over the mirror (for the SSR explorer / CLI).
    pub fn store(&self) -> &LocalFolderStore {
        &self.store
    }

    /// Re-sync the mirror to the remote and rebuild the graph.
    pub fn pull(&mut self) -> Result<()> {
        clone_or_update(&self.remote, &self.dest, self.token.as_deref())?;
        self.store.reload()?;
        Ok(())
    }
}

/// Bring the mirror at `dest` up to date with `remote`, cloning it first if
/// `dest` does not exist.
///
/// The update is a hard reset, which is only safe on a directory this crate
/// created for this remote: [`ensure_owned_mirror`] refuses anything else, so a
/// mistaken `--cache` pointing at a real clone can never lose its work.
fn clone_or_update(remote: &str, dest: &Path, token: Option<&str>) -> Result<()> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| GithubError::BadRepo(dest.display().to_string()))?;
    let empty_dir = dest.is_dir() && std::fs::read_dir(dest)?.next().is_none();
    if dest.exists() && !empty_dir {
        ensure_owned_mirror(dest, remote)?;
        run_git(&["-C", dest_str, "fetch", "--prune", "origin"], token)?;
        // Managed mirror, proven ours above: reset to the tracked upstream.
        run_git(&["-C", dest_str, "reset", "--hard", "@{u}"], token)?;
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // `git clone` accepts an empty directory as its destination.
        // `--` so a remote beginning with `-` can never be read as an option.
        run_git(&["clone", "--", remote, dest_str], token)?;
        std::fs::write(dest.join(".git").join(MIRROR_MARKER), remote)?;
    }
    Ok(())
}

/// Succeed only if `dest` is a mirror this crate cloned from `remote`: it must
/// carry the marker written at clone time, the marker must name `remote`, and
/// git's own `origin` must agree.
fn ensure_owned_mirror(dest: &Path, remote: &str) -> Result<()> {
    let marker = dest.join(".git").join(MIRROR_MARKER);
    let recorded = std::fs::read_to_string(&marker).map_err(|_| {
        GithubError::NotOurMirror(format!("{} has no BerryWiki mirror marker", dest.display()))
    })?;
    if recorded != remote {
        return Err(GithubError::NotOurMirror(format!(
            "{} mirrors {recorded:?}, not {remote:?}",
            dest.display()
        )));
    }
    let out = Command::new("git")
        .args(["-C"])
        .arg(dest)
        .args(["config", "--get", "remote.origin.url"])
        .output()?;
    let origin = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if origin != remote {
        return Err(GithubError::NotOurMirror(format!(
            "{}'s origin is {origin:?}, not {remote:?}",
            dest.display()
        )));
    }
    Ok(())
}

/// Run one git command, supplying `token` (if any) through a private askpass
/// helper and never following an HTTP redirect while a token is in play.
fn run_git(args: &[&str], token: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("git");
    if token.is_some() {
        // A redirect is where a credential leaves its intended host: git would
        // ask the helper again for the new host and hand it the token.
        cmd.args(["-c", "http.followRedirects=false"]);
    }
    cmd.args(args);
    // Never block on an interactive credential prompt.
    cmd.env("GIT_TERMINAL_PROMPT", "0");

    // Keep the askpass temp file alive for the duration of the command.
    #[cfg(unix)]
    let _askpass = if let Some(t) = token {
        let guard = write_askpass()?;
        cmd.env("GIT_ASKPASS", guard.path());
        cmd.env("BERRYWIKI_TOKEN", t);
        cmd.env("GIT_USERNAME", "x-access-token");
        Some(guard)
    } else {
        None
    };
    #[cfg(not(unix))]
    let _ = token; // token auth is only wired for unix hosts (the estate target)

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GithubError::Git {
            context: format!("git {}", args.first().copied().unwrap_or("")),
            stderr: redact_token(&stderr, token),
        });
    }
    Ok(())
}

/// An askpass helper in its own private directory; both are removed on drop.
#[cfg(unix)]
struct AskpassGuard {
    dir: PathBuf,
    script: PathBuf,
}

#[cfg(unix)]
impl AskpassGuard {
    fn path(&self) -> &Path {
        &self.script
    }
}

#[cfg(unix)]
impl Drop for AskpassGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.script);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// Write the askpass helper into a freshly created, owner-only directory.
///
/// Both the directory and the file are created exclusively: if either already
/// exists (another user pre-planted it to capture or run code with the token)
/// this fails rather than reusing it.
#[cfg(unix)]
fn write_askpass() -> Result<AskpassGuard> {
    write_askpass_in(&std::env::temp_dir())
}

/// [`write_askpass`] under an explicit base directory (tests plant hazards there).
#[cfg(unix)]
fn write_askpass_in(base: &Path) -> Result<AskpassGuard> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = base.join(format!(
        "berrywiki-askpass-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let script = dir.join("askpass.sh");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&script)?;
    f.write_all(askpass_script().as_bytes())?;
    Ok(AskpassGuard { dir, script })
}

#[cfg(test)]
mod tests {
    use super::*;
    use berrywiki_git_compat::GitSandbox;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static C: AtomicUsize = AtomicUsize::new(0);

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/test-wiki")
            .canonicalize()
            .unwrap()
    }

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "berrywiki-gh-{tag}-{}-{}",
            std::process::id(),
            C.fetch_add(1, Ordering::SeqCst)
        ))
    }

    #[test]
    fn resolves_wiki_urls() {
        assert_eq!(
            wiki_git_url("octocat/hello").unwrap(),
            "https://github.com/octocat/hello.wiki.git"
        );
        assert_eq!(
            wiki_git_url("https://github.com/octocat/hello").unwrap(),
            "https://github.com/octocat/hello.wiki.git"
        );
        assert_eq!(
            wiki_git_url("https://github.com/octocat/hello.git").unwrap(),
            "https://github.com/octocat/hello.wiki.git"
        );
        assert_eq!(
            wiki_git_url("https://github.com/octocat/hello.wiki.git").unwrap(),
            "https://github.com/octocat/hello.wiki.git"
        );
        // Local bare repo paths pass through unchanged — including the
        // two-path-segment form that previously looked like an owner/name slug.
        assert_eq!(
            wiki_git_url("/tmp/x/remote.git").unwrap(),
            "/tmp/x/remote.git"
        );
        assert_eq!(wiki_git_url("/tmp/bare.git").unwrap(), "/tmp/bare.git");
        assert_eq!(wiki_git_url("./local.git").unwrap(), "./local.git");
        assert!(wiki_git_url("").is_err());
    }

    #[test]
    fn redaction_hides_token() {
        assert_eq!(
            redact_token("auth secret123 fail", Some("secret123")),
            "auth *** fail"
        );
        assert_eq!(redact_token("no token here", None), "no token here");
    }

    #[test]
    fn askpass_distinguishes_username_and_password() {
        let s = askpass_script();
        assert!(s.contains("GIT_USERNAME"));
        assert!(s.contains("BERRYWIKI_TOKEN"));
        assert!(
            s.contains("sername"),
            "handles the Username prompt distinctly"
        );
    }

    #[test]
    fn clones_a_bare_remote_and_reads_pages() {
        // A local bare repo stands in for the .wiki.git remote (no GitHub).
        let sandbox = GitSandbox::create(&fixture());
        let dest = scratch("clone");
        let wiki = GitHubWiki::open(sandbox.remote.to_str().unwrap(), &dest, None).unwrap();
        let pages = wiki.store().list_pages();
        assert!(pages.iter().any(|p| p.title == "Home"));
        assert_eq!(pages.len(), 10);
    }

    #[test]
    fn pull_picks_up_remote_changes() {
        let sandbox = GitSandbox::create(&fixture());
        let dest = scratch("pull");
        let mut wiki = GitHubWiki::open(sandbox.remote.to_str().unwrap(), &dest, None).unwrap();

        // Someone edits the wiki on the "remote" side and pushes.
        sandbox.commit_change(
            &sandbox.theirs,
            "New-Remote-Page.md",
            "# New Remote Page\n\nadded upstream\n",
            "Add page",
        );
        sandbox
            .git(&sandbox.theirs, &["push", "origin", "main"])
            .expect_success("push");

        // Before pull: not visible. After pull: visible.
        assert!(!wiki
            .store()
            .list_pages()
            .iter()
            .any(|p| p.title == "New Remote Page"));
        wiki.pull().unwrap();
        assert!(wiki
            .store()
            .list_pages()
            .iter()
            .any(|p| p.title == "New Remote Page"));
    }

    #[test]
    fn open_missing_remote_errors_without_panicking() {
        let dest = scratch("missing");
        match GitHubWiki::open("/no/such/bare-repo.git", &dest, None) {
            Err(GithubError::Git { .. }) => {}
            Err(other) => panic!("expected a git error, got {other}"),
            Ok(_) => panic!("cloning a nonexistent remote must fail"),
        }
    }

    #[test]
    fn tokens_go_only_to_https_github() {
        for ok in [
            "https://github.com/octocat/hello.wiki.git",
            "https://GitHub.com/octocat/hello.wiki.git",
        ] {
            assert!(token_permitted_for(ok), "{ok}");
        }
        for bad in [
            "http://github.com/octocat/hello.wiki.git",
            "https://github.com.evil.example/x.wiki.git",
            "https://evil.example/github.com/x.wiki.git",
            "https://user:pw@github.com/x.wiki.git",
            "https://github.com:8443/x.wiki.git",
            "https://github.com",
            "git@github.com:octocat/hello.wiki.git",
            "/tmp/bare.git",
        ] {
            assert!(!token_permitted_for(bad), "{bad}");
        }
    }

    #[test]
    fn a_token_for_a_non_github_remote_is_refused_before_any_git_runs() {
        let sandbox = GitSandbox::create(&fixture());
        let dest = scratch("tokref");
        let planted = format!("test-fixture-only-{}", std::process::id());
        match GitHubWiki::open(sandbox.remote.to_str().unwrap(), &dest, Some(&planted)) {
            Err(GithubError::TokenRefused(_)) => {}
            Err(other) => panic!("expected TokenRefused, got {other}"),
            Ok(_) => panic!("a token must not be offered to a non-GitHub remote"),
        }
        assert!(!dest.exists(), "nothing may be cloned on refusal");
    }

    #[test]
    fn a_real_clone_used_as_cache_is_refused_and_its_work_survives() {
        let sandbox = GitSandbox::create(&fixture());
        // `ours` is an ordinary working clone with an uncommitted edit.
        let precious = sandbox.ours.join("Unsaved-Work.md");
        std::fs::write(&precious, "# Unsaved\n\nnot committed anywhere\n").unwrap();
        match GitHubWiki::open(sandbox.remote.to_str().unwrap(), &sandbox.ours, None) {
            Err(GithubError::NotOurMirror(_)) => {}
            Err(other) => panic!("expected NotOurMirror, got {other}"),
            Ok(_) => panic!("a clone BerryWiki did not create must not be reset"),
        }
        assert_eq!(
            std::fs::read_to_string(&precious).unwrap(),
            "# Unsaved\n\nnot committed anywhere\n"
        );
    }

    #[test]
    fn a_mirror_of_another_remote_is_refused() {
        let a = GitSandbox::create(&fixture());
        let b = GitSandbox::create(&fixture());
        let dest = scratch("swap");
        GitHubWiki::open(a.remote.to_str().unwrap(), &dest, None).unwrap();
        match GitHubWiki::open(b.remote.to_str().unwrap(), &dest, None) {
            Err(GithubError::NotOurMirror(_)) => {}
            Err(other) => panic!("expected NotOurMirror, got {other}"),
            Ok(_) => panic!("a mirror of one remote must not be reset to another"),
        }
        // Positive control: reopening for its own remote still works.
        GitHubWiki::open(a.remote.to_str().unwrap(), &dest, None).unwrap();
    }

    #[test]
    fn an_empty_cache_directory_is_cloned_into() {
        let sandbox = GitSandbox::create(&fixture());
        let dest = scratch("empty");
        std::fs::create_dir_all(&dest).unwrap();
        let wiki = GitHubWiki::open(sandbox.remote.to_str().unwrap(), &dest, None).unwrap();
        assert!(wiki.store().list_pages().iter().any(|p| p.title == "Home"));
    }

    #[cfg(unix)]
    #[test]
    fn the_askpass_helper_is_private_and_cannot_be_preplanted() {
        use std::os::unix::fs::PermissionsExt;
        let base = scratch("askbase");
        std::fs::create_dir_all(&base).unwrap();
        let guard = write_askpass_in(&base).unwrap();
        let dir_mode = std::fs::metadata(guard.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o077, 0, "helper dir must be owner-only");
        let path = guard.path().to_path_buf();
        drop(guard);
        assert!(!path.exists(), "helper is removed after use");

        // Plant the next directory name an attacker would predict.
        let planted_any =
            (0..64).map(|k| base.join(format!("berrywiki-askpass-{}-{k}", std::process::id())));
        for p in planted_any {
            let _ = std::fs::create_dir(&p);
        }
        assert!(
            write_askpass_in(&base).is_err(),
            "a pre-existing helper directory must be refused, not reused"
        );
    }

    /// A one-shot HTTP responder: answers every connection with `reply` and
    /// records each request head it saw. Runs until the test process exits.
    fn http_stub(reply: String) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut head = String::new();
                let mut r = BufReader::new(s.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                log.lock().unwrap().push(head);
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (addr, seen)
    }

    #[cfg(unix)]
    #[test]
    fn a_redirect_never_carries_the_token_to_another_host() {
        // Synthetic credential generated for this local test (the #48 pattern):
        // it authenticates nothing, and only loopback stub servers see it.
        let planted = format!("test-fixture-only-{}", std::process::id());
        // The redirect target demands credentials and records what it gets.
        let (target, target_seen) = http_stub(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\n\
Content-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string(),
        );
        let leaked = |seen: &std::sync::Arc<std::sync::Mutex<Vec<String>>>| {
            seen.lock()
                .unwrap()
                .iter()
                .any(|h| h.to_ascii_lowercase().contains("authorization:"))
        };
        // The first host only redirects to the target.
        let redirect = |dest: &str| {
            format!(
                "HTTP/1.1 301 Moved\r\nLocation: http://{dest}/x.wiki.git/info/refs?service=git-upload-pack\r\n\
Content-Length: 0\r\nConnection: close\r\n\r\n"
            )
        };
        let (first, _) = http_stub(redirect(&target));
        let url = format!("http://{first}/x.wiki.git");

        // Through run_git: the redirect is not followed, so nothing reaches
        // the target at all, credentials included.
        let r = run_git(&["ls-remote", "--", &url], Some(planted.as_str()));
        assert!(r.is_err(), "a refused redirect must surface as an error");
        assert!(
            !leaked(&target_seen),
            "the token reached the redirect target"
        );
        if let Err(e) = r {
            assert!(
                !e.to_string().contains(&planted),
                "error text leaks the token"
            );
        }

        // Positive control: the same request with redirects allowed DOES hand
        // the token to the target, so the check above can observe a leak.
        let (target2, target2_seen) = http_stub(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\n\
Content-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string(),
        );
        let (first2, _) = http_stub(redirect(&target2));
        let guard = write_askpass().unwrap();
        let _ = Command::new("git")
            .args(["-c", "http.followRedirects=true", "ls-remote", "--"])
            .arg(format!("http://{first2}/x.wiki.git"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", guard.path())
            .env("BERRYWIKI_TOKEN", &planted)
            .env("GIT_USERNAME", "x-access-token")
            .output()
            .unwrap();
        assert!(
            leaked(&target2_seen),
            "control failed: with redirects followed, the target should have received credentials"
        );
    }
}
