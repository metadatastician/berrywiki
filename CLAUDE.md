# BerryWiki — Claude Code Project Instructions

BerryWiki gives GitHub.com Wikis a CherryTree/Zim-style hierarchical notebook
while keeping content as plain, GitHub-renderable Markdown in the `.wiki.git`
repo. **The wiki must stay fully usable without BerryWiki.**

## Locked constraints (violations are bugs, not preferences)

- **TypeScript and hand-written JavaScript are banned.** Core = Rust
  (→ Rust/SPARK proofs later). Web UI = AffineScript→typed-wasm, deferred
  until that language matures. See ADR-0003.
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
- WSL Rust is distro 1.85 at `/usr/bin` — **no rustup/rustfmt/clippy
  locally**; lint gates run in CI. No `rsync` (use `cp`). `gh` not installed.
- Long commit messages: write to a temp file, `git commit -F` (PowerShell
  32k command-line limit).

## Build & verify

```sh
cargo test --workspace          # must pass before any report of completion
cargo build --workspace        # must be warning-free
```

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
