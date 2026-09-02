# AGENTS.md — instructions for coding agents

This file is the entry point for agents that read `AGENTS.md` (Codex, Gemini
CLI, Copilot and others). Claude Code reads `CLAUDE.md`, which carries the same
constraints plus model-routing detail — **the two must not diverge**; when you
change one, change the other.

BerryWiki gives GitHub.com Wikis a CherryTree/Zim-style hierarchical notebook
while keeping content as plain, GitHub-renderable Markdown in the `.wiki.git`
repo. **The wiki must stay fully usable without BerryWiki.**

## Locked constraints — violations are bugs, not preferences

- **No TypeScript or hand-written JavaScript** (ADR-0003). The ban covers
  everything shipped or served: no `<script>` in any response, no JS/TS in any
  crate. Local agent tooling (`.claude/workflows/*.js`) is out of scope.
  Whether *generated* client script may ever ship is **ADR-0007, unruled** —
  do not assume either answer, and do not write code that presumes one.
- **Hierarchy lives in metadata, never in filenames** (ADR-0001). Flat
  `--`-separated filenames are derived from titles; the tree comes from the
  `<!-- berrywiki -->` block and the generated `_Sidebar.md`.
- **App state never enters the wiki clone** (ADR-0008). Drafts, journal and
  index live under the XDG state home, so a plain-git user cannot commit them.
- **Git safety:** never force-push, never discard local work, fetch before
  push, atomic logical commits, sidebar regenerated with the tree change.
- **Determinism:** metadata serialisation is idempotent; sidebar output is
  byte-deterministic. Re-saving an unchanged page must produce **no diff** —
  this is why submitted text is CRLF-normalised before it reaches the store.
- **Malformed input degrades with a diagnostic.** Never panic, never destroy
  content. Unknown metadata fields are preserved verbatim.
- **Honesty:** never claim live GitHub behaviour is verified unless it was
  actually tested against a real wiki. Everything in
  `docs/compatibility/github-wiki.adoc` is currently **unverified**; the
  spikes that would verify it are credential-gated.
- **Docs:** AsciiDoc for technical docs and ADRs; Markdown only for wiki
  content and community-health files.

## Build and verify

```sh
cargo test --workspace     # must pass before reporting anything complete
cargo build --workspace    # must be warning-free
```

`cargo test` **is** the safety gate: it carries the script-free SSR assertions
and the git conflict / no-data-loss harness. Do not report work complete on a
build alone.

Host Rust is a rustup toolchain (1.97, newer than the 1.89 MSRV) with rustfmt
and clippy installed. Run them on the host, then repeat them plus a build under
the MSRV toolchain in a container, because 1.89 can reject what 1.97 accepts:

```sh
podman run --rm -v "$PWD":/work -w /work docker.io/library/rust:1.89-slim sh -c \
  'rustup component add clippy rustfmt >/dev/null 2>&1
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo build --workspace'
```

CI denies warnings. Check runs with `gh run list`, never `gh pr checks` —
the latter hides `startup_failure`.

## Where things belong

| Change | Crate |
|---|---|
| Parsing, hierarchy, backlinks, diagnostics, sidebar | `berrywiki-core` (no I/O) |
| Anything that writes wiki content | `berrywiki-store` — nowhere else |
| Out-of-clone state, journal | `berrywiki-appstate`, `berrywiki-draft` |
| git operations | `berrywiki-git` (closed set), `berrywiki-sync` |
| UI, routes, HTML | `berrywiki-serve` — **no third-party dependencies** |

New routes must be added to the script-free test sweep in
`crates/berrywiki-serve/tests/editor.rs`.

## Before starting work

Read `docs/execution/work-packages.adoc` — work is pre-packaged, and each
package records what blocks it. Known debt is in
`docs/execution/debt-register.adoc`. Design changes that touch a locked
constraint need an ADR in `docs/decisions/`, not a pull request.
