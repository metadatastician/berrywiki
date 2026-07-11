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
//! # Credentials & honesty
//!
//! Public wikis clone anonymously. Private wikis authenticate with a token via
//! `GIT_ASKPASS` (never embedded in a logged URL; redacted from any error). The
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
    /// A `git` invocation failed. `stderr` has the token redacted.
    Git { context: String, stderr: String },
    Store(StoreError),
    Io(std::io::Error),
}

impl std::fmt::Display for GithubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GithubError::BadRepo(r) => write!(f, "Not a usable wiki target: {r:?}."),
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
        return Ok(format!("https://github.com/{}/{}.wiki.git", segs[0], segs[1]));
    }
    // Otherwise treat as a direct remote (local bare repo, other URL form).
    Ok(r.to_string())
}

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

fn clone_or_update(remote: &str, dest: &Path, token: Option<&str>) -> Result<()> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| GithubError::BadRepo(dest.display().to_string()))?;
    if dest.join(".git").is_dir() {
        run_git(&["-C", dest_str, "fetch", "--prune", "origin"], token)?;
        // Managed mirror: reset to the tracked upstream. The cache dir holds no
        // user edits (editing is Phase 2/3 with a different strategy).
        run_git(&["-C", dest_str, "reset", "--hard", "@{u}"], token)?;
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        run_git(&["clone", remote, dest_str], token)?;
    }
    Ok(())
}

fn run_git(args: &[&str], token: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("git");
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

#[cfg(unix)]
struct AskpassGuard(PathBuf);

#[cfg(unix)]
impl AskpassGuard {
    fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(unix)]
impl Drop for AskpassGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(unix)]
fn write_askpass() -> Result<AskpassGuard> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "berrywiki-askpass-{}-{}.sh",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o700)
        .open(&path)?;
    f.write_all(askpass_script().as_bytes())?;
    Ok(AskpassGuard(path))
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
        assert_eq!(wiki_git_url("/tmp/x/remote.git").unwrap(), "/tmp/x/remote.git");
        assert_eq!(wiki_git_url("/tmp/bare.git").unwrap(), "/tmp/bare.git");
        assert_eq!(wiki_git_url("./local.git").unwrap(), "./local.git");
        assert!(wiki_git_url("").is_err());
    }

    #[test]
    fn redaction_hides_token() {
        assert_eq!(redact_token("auth secret123 fail", Some("secret123")), "auth *** fail");
        assert_eq!(redact_token("no token here", None), "no token here");
    }

    #[test]
    fn askpass_distinguishes_username_and_password() {
        let s = askpass_script();
        assert!(s.contains("GIT_USERNAME"));
        assert!(s.contains("BERRYWIKI_TOKEN"));
        assert!(s.contains("sername"), "handles the Username prompt distinctly");
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
        assert!(!wiki.store().list_pages().iter().any(|p| p.title == "New Remote Page"));
        wiki.pull().unwrap();
        assert!(wiki.store().list_pages().iter().any(|p| p.title == "New Remote Page"));
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
}
