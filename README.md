# BerryWiki

A companion authoring, navigation, indexing and synchronisation layer for
**GitHub.com Wikis** that gives them a CherryTree/Zim-style hierarchical
notebook experience — while keeping the content as plain, GitHub-compatible
Markdown in ordinary Git storage.

> **Core principle:** the wiki must stay fully usable when BerryWiki is not.
> You can always clone the `.wiki.git` repo, read/edit the Markdown in any
> editor, commit, push, and view it through GitHub's normal Wiki UI.
> BerryWiki *enhances* the files; it never makes them depend on the app.

## What works today

A local wiki folder can be browsed and edited end to end:

```sh
berrywiki serve ./my-wiki          # three-pane explorer + editor at :23779
```

* **Hierarchical notebook over flat files.** Tree, sibling ordering, backlinks
  and a generated `_Sidebar.md` that GitHub renders natively — all driven by a
  hidden metadata block that is invisible in the rendered wiki, never by
  filenames.
* **Zero-JavaScript editor.** Source editing with preview, page create and
  delete, explicit **Save** and **Save-draft**. Drafts live outside the clone,
  survive a killed process, and are visible as a banner, a badge and a marker in
  the tree. No `<script>` is served anywhere — a test asserts it on every route.
* **Writes that refuse rather than clobber.** If a page changed on disk, or
  changed since your editor was opened, Save is refused with a 409 — and your
  text is kept, both in the form and as a draft. Nothing you typed is discarded.
* **Consistency diagnostics.** Broken links, missing parents, cycles and
  duplicate ids, surfaced in the UI and via `berrywiki check`.
* **Commit-on-save** (`berrywiki-sync`, wired into `serve`): when the folder
  is a git working tree, every save, create and delete is one atomic commit
  with the sidebar in the same commit. Changes made outside BerryWiki are
  checkpointed as their own commit first, never clobbered. `/changes` lists
  unpublished commits and offers fetch + fast-forward + push; if the branch
  has diverged, `/conflicts` hands off with the exact `git` steps. Never
  force-pushes, never merges for you, never discards local work.
  `serve --no-commit` serves the folder without touching git.

## What does not work yet

Being explicit, because the difference matters:

* **In-app conflict resolution** — divergence is detected and handed off
  (`/conflicts`), but merging inside BerryWiki (P3-conflict) does not exist;
  you resolve it with git.
* **Subtree move/rename has no UI yet.** The engine is done: a non-leaf move
  cascades descendant filenames, rewrites inbound links, regenerates the
  sidebar and lands as one commit, and a crashed move is recovered on open,
  all under test. The editor route with a dry-run preview is P2-move.
* **GitHub serving is read-only.** `serve --github` mirrors a wiki and renders
  no edit affordances.
* **Live GitHub behaviour is unverified.** Every GitHub Wiki behaviour BerryWiki
  relies on is recorded in [`docs/compatibility/github-wiki.adoc`](docs/compatibility/github-wiki.adoc)
  and, as of today, **none has been tested against a real wiki** — those spikes
  are credential-gated. Treat the compatibility report as a hypothesis list.
* **No importers, no packaging, no proofs yet** — CherryTree/Zim import and
  Guix packaging are Phase 5. The invariants a proof would cover are written
  down in [`docs/proofs/invariants.adoc`](docs/proofs/invariants.adoc), each
  with the tests that witness it and a CI gate that keeps the list honest.
  They are tested, not proved.

Current position: Phases 0–3 largely built, Phase 4–5 open. See
[`docs/execution/work-packages.adoc`](docs/execution/work-packages.adoc) for the
package-by-package state and [`docs/execution/debt-register.adoc`](docs/execution/debt-register.adoc)
for known debt.

## Install and use

Requires Rust 1.89 or newer. No other runtime.

```sh
cargo build --release
./target/release/berrywiki --help
```

```sh
berrywiki check ./my-wiki           # tree + diagnostics; exit 1 on any error
berrywiki sidebar ./my-wiki --write # regenerate _Sidebar.md
berrywiki serve ./my-wiki           # browse and edit at http://127.0.0.1:23779
berrywiki serve ./my-wiki --no-commit           # same, without commit-on-save
berrywiki serve ./my-wiki --author "Ada <ada@example.org>"   # commit identity
berrywiki serve --github owner/repo # mirror a GitHub wiki (read-only)
```

For a private wiki, supply a token via `BERRYWIKI_GITHUB_TOKEN` — never as a
command-line argument, so it stays out of shell history and process listings.

`fixtures/test-wiki/` is a small notebook you can point any of these at.

## How a page looks on disk

Ordinary Markdown, preceded by a comment GitHub does not render:

```markdown
<!-- berrywiki
id: 0195f6ec-36a2-7a42-b519-5f558842e256
parent: 0195f6d0-b787-7c3a-a48f-c1a04fb2ea84
position: 30
kind: page
tags:
  - assessment
-->

# Assessment Plan

Ordinary Markdown from here on.
```

Delete the comment and the page is still a perfectly good wiki page — it simply
stops being part of the tree. That is the point.

## Architecture

Eleven crates, layered so the parts that must be provably correct have no I/O
to be wrong about — see [`ARCHITECTURE.md`](ARCHITECTURE.md).

* **Engine:** Rust. No JavaScript or TypeScript, hand-written or generated
  (ADR-0003); the UI is server-rendered and script-free by test.
* **`berrywiki-serve`** has no third-party dependencies at all — a hand-rolled
  `std::net` server, no async runtime, no web framework.
* **Docs:** AsciiDoc (`.adoc`) for technical docs and ADRs; Markdown (`.md`) for
  wiki content and community-health files.

## Layout

```
crates/berrywiki-core/      deterministic engine (no I/O)
crates/berrywiki-store/     WikiStore trait + LocalFolderStore (atomic writes)
crates/berrywiki-serve/     zero-JS three-pane explorer and editor
crates/berrywiki-git/       closed-set git wrapper · -sync/-github/-git-compat
crates/berrywiki-appstate/  out-of-clone app state · -draft for drafts
crates/berrywiki-cli/       the `berrywiki` command
fixtures/test-wiki/         fixture notebook (Markdown)
docs/architecture/          plan + overview
docs/compatibility/         GitHub Wiki compatibility findings (unverified)
docs/decisions/             architecture decision records
docs/execution/             work packages + debt register
docs/proofs/                invariants ledger INV-1..6 (tested, proof scheduled)
scripts/                    CI gates (invariants-ledger check)
```

## Build & test

```sh
cargo test --workspace     # includes the no-<script> and no-data-loss harnesses
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo test` *is* the safety gate: it carries the script-free SSR assertions and
the git conflict / no-data-loss harness.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Security reports:
[`SECURITY.md`](SECURITY.md).

## Licence

Code is licensed under **MPL-2.0**; documentation under **CC-BY-SA-4.0**.
Full texts in [`LICENSES/`](LICENSES/); machine-readable mapping in
[`REUSE.toml`](REUSE.toml).
