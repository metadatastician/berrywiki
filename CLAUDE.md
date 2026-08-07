# BerryWiki — Claude Code Project Instructions

BerryWiki gives GitHub.com Wikis a CherryTree/Zim-style hierarchical notebook
while keeping content as plain, GitHub-renderable Markdown in the `.wiki.git`
repo. **The wiki must stay fully usable without BerryWiki.**

## Locked constraints (violations are bugs, not preferences)

- **TypeScript and hand-written JavaScript are banned.** Core = Rust
  (→ Rust/SPARK proofs later). Web UI = AffineScript→typed-wasm, deferred
  until that language matures. See ADR-0003.
  *Scope:* the ban covers everything **shipped or served** — no `<script>` in
  any response, no JS/TS in any crate. It does not cover local agent tooling
  (`.claude/workflows/*.js` is a Claude Code harness, never distributed).
  Whether *generated* client script may ever ship is ADR-0007, **unruled** —
  do not assume either answer.
- **Native GitHub wiki reader must work.** Flat `--`-separated filenames
  (ADR-0001, provisional); tree lives in `<!-- berrywiki -->` metadata +
  generated `_Sidebar.md`, never inferred from filenames.
- **Git safety:** never force-push, never discard local work, fetch before
  push, atomic logical commits, sidebar regenerated in the same commit as the
  tree change. Evidence base: `crates/berrywiki-git-compat`.
- **Honesty:** never claim live GitHub behaviour is verified unless actually
  tested against a real wiki (currently ALL unverified — see
  `docs/compatibility/github-wiki.adoc`). Live tests are credential-gated.
- **Docs:** AsciiDoc for technical docs/ADRs; Markdown only for wiki content
  and community-health files.
- **Determinism:** metadata serialisation is idempotent; sidebar output is
  byte-deterministic; derived data (graph, index) is always rebuildable.
- Malformed input degrades with a diagnostic; it never panics or destroys
  content. Unknown metadata fields are preserved.

## Environment gotchas (this machine)

- The repo lives in WSL at `~/developer/meta-repos/berrywiki`. **Only WSL
  tooling may write to it or run git in it** — Windows git/editors cause
  clone desync. Author files Windows-side, `cp` in from inside WSL.
- WSL Rust is distro 1.85 at `/usr/bin` — **no rustup, so no rustfmt/clippy
  on the host**. Do not skip those gates: run them in a container instead
  (recipe below). No `rsync` (use `cp`).
- `gh` **is** installed and authenticated.
- Long commit messages: write to a temp file and `git commit -F`.

## Build & verify

```sh
cargo test --workspace          # must pass before any report of completion
cargo build --workspace         # must be warning-free
```

`cargo test` IS the safety gate — it carries the script-free SSR assertions
and the git conflict / no-data-loss harness.

rustfmt and clippy have no host toolchain, so run them through podman before
pushing (CI denies warnings, and a first-ever lint run finds real problems):

```sh
podman run --rm -v "$PWD":/work -w /work docker.io/library/rust:1.86-slim sh -c \
  'rustup component add clippy rustfmt >/dev/null 2>&1
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings'
```

Note the container toolchain is older than CI's `stable`, so it can miss a
newer lint. Check the PR run with `gh run list` — never `gh pr checks`, which
hides `startup_failure`.

## Model routing for execution

Work is pre-packaged in `docs/execution/work-packages.adoc`, each tagged with
the Claude tier that should execute it:

- **Opus** — irreversible or subtle stages: sync engine, rich-editor
  round-tripping, conflict UX, transactional move + link rewriting,
  CherryTree conversion semantics, SPARK proofs, live-spike interpretation.
- **Sonnet** — well-scoped builds against a settled design: search index,
  autocomplete, UI panes per ADR-0005, Zim importer plumbing, CI/packaging.
- **Haiku** — mechanical, checklist-driven sweeps: fixtures, doc sync,
  lint fixes, report formatting. Haiku agents must **stop and escalate**
  rather than improvise when a package's escalation triggers fire.

Agent definitions with these tiers live in `.claude/agents/`. The
`berrywiki-slice` workflow (`.claude/workflows/`) runs a package through
design → implement → verify with the right tier per stage.
