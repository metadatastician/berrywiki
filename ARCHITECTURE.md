# BerryWiki Architecture

BerryWiki is a companion layer for **GitHub.com Wikis**. It gives a wiki a
CherryTree/Zim-style hierarchical notebook — tree navigation, backlinks, a
generated sidebar, an editor — while the content stays plain, GitHub-renderable
Markdown in the ordinary `<repo>.wiki.git` repository.

> **The constraint that shapes everything below:** the wiki must stay fully
> usable when BerryWiki is not. Clone the `.wiki.git` repo, edit the Markdown in
> any editor, commit, push, read it through GitHub's own Wiki UI — all of that
> must keep working. BerryWiki *enhances* the files; it never makes them depend
> on the app. Any design that would break plain-git usage is rejected, not
> worked around.

For the reasoning behind each decision, see `docs/decisions/` (ADRs). For what
is built and what is not, see `docs/execution/work-packages.adoc`. Known debt is
tracked in `docs/execution/debt-register.adoc`.

## Where the truth lives

| Layer | What it holds | Rebuildable? |
|---|---|---|
| Markdown files in `.wiki.git` | **Canonical.** Page content, and — in a hidden HTML comment — page id, parent, position, kind, tags. | No. This is the data. |
| `_Sidebar.md` | Generated navigation GitHub renders natively. | Yes, deterministically. |
| Page graph, backlinks, diagnostics | In-memory derived index. | Yes, from the files alone. |
| Drafts, journal, search index | App-private state, kept **outside** the clone. | Yes, and losing it costs at most unsaved drafts. |

Two consequences follow, and they explain most of the code:

* **Identity is a stable id, not a filename.** Filenames are derived from titles
  (ADR-0001), so renaming a page changes its filename; hierarchy is carried in
  the metadata block instead, and never inferred from the name.
* **App state never enters the clone** (ADR-0008). If drafts lived beside the
  pages, a plain-git user would commit and push them — a breach of the core
  principle above.

## The crates

Eleven crates, layered so that the parts which must be provably correct have no
I/O to be wrong about.

### Engine — no I/O at all

* **`berrywiki-core`** — the deterministic engine. Parses and serialises the
  hidden metadata block (idempotently: re-saving an unchanged page produces
  byte-identical output), extracts headings and links, builds the page
  hierarchy, computes backlinks, emits consistency diagnostics (broken links,
  missing parents, cycles, duplicate ids), and generates `_Sidebar.md`. It
  touches no filesystem, so every rule it encodes is testable in-process.

### Storage — the only layer that writes wiki content

* **`berrywiki-store`** — the `WikiStore` trait plus `LocalFolderStore`. Writes
  are atomic (temp file + rename, so no reader sees a partial page); paths are
  validated component-wise against traversal and Windows-hostile names; a move
  writes and verifies the new file before removing the old, so a crash leaves
  both files (a visible duplicate-id diagnostic), never neither. Every mutation
  regenerates the sidebar in the same operation, so the tree change and its
  navigation land together. A move is first *planned* as a pure dry run
  (`plan_move`) and then applied from that plan, so the editor's preview and
  the real move cannot disagree.
* **`berrywiki-appstate`** — resolves the out-of-clone home under
  `$XDG_STATE_HOME/berrywiki/<repo-id>/`, keyed by a stable hash of the wiki
  path, holds the roll-forward operation journal, and owns the single-writer
  `RepoLock` (an OS advisory lock, released by the OS on process death) that
  `serve` holds for its lifetime and CLI writes take for their duration.
* **`berrywiki-draft`** — per-page drafts, atomically written, stored in that
  out-of-clone home.

### Git and GitHub

* **`berrywiki-git`** — a deliberately small wrapper over a *closed* set of git
  CLI operations. No force-push, no `reset --hard`, no blanket working-tree
  discard; fetch before push.
* **`berrywiki-sync`** — turns each completed store mutation into one atomic
  logical commit. Wired into `serve` as the default editor backend
  (commit-on-save, `/changes`, `/conflicts`, `POST /sync`); `--no-commit`
  bypasses it and the editor writes files only.
  It never starts a merge, and a mutation is refused while one is unfinished.
  When the operator has started a merge, it classifies each conflicted path
  from index stages 1, 2 and 3 (ancestor, ours, theirs) rather than from the
  working tree, which is what makes the body-versus-metadata distinction
  decidable. Only a clash in the generated `_Sidebar.md` is settled, by
  regenerating it from the settled page set; a merge whose every clash is that
  file can be concluded and its merge commit recorded. Page bodies and
  metadata are never merged: all three sides are shown and the operator
  settles them in git.
* **`berrywiki-github`** — clones/updates a `<repo>.wiki.git` working copy and
  exposes it as a store. Read-only by construction: it is a hard-reset mirror,
  fenced off from the editor; the write path is always `berrywiki-sync` over an
  ordinary clone.
* **`berrywiki-git-compat`** — an evidence harness, not a feature: it
  reproduces remote-change, non-fast-forward and merge-conflict situations and
  asserts no data is lost. It exists so the safety claims above are tested
  rather than asserted.

### Interface

* **`berrywiki-render`** — Markdown → HTML via comrak (GFM), with raw HTML
  escaped and dangerous URL schemes neutralised.
* **`berrywiki-serve`** — the three-pane explorer and editor: tree | page |
  outline & backlinks. **Zero JavaScript** (ADR-0003, ADR-0005) — plain HTML
  forms, server-rendered, on a hand-rolled `std::net` server with no async
  runtime and no third-party dependencies. Editing uses explicit Save and
  Save-draft with a two-layer stale-write guard; a refused save never discards
  what you typed.
* **`berrywiki-cli`** — `berrywiki check | sidebar | serve | backup | restore`.

```
                 berrywiki-cli
                       │
                 berrywiki-serve ──── berrywiki-github
                 (+ -render, -draft)   (--github: read-only mirror)
                       │
                 berrywiki-sync   (--no-commit skips this layer)
                       │
                 berrywiki-git
                       │
             berrywiki-store ── berrywiki-appstate
                   │                 (out-of-clone state)
             berrywiki-core
             (no I/O; the rules)
```

## Technology, and what is deliberately absent

* **Rust** for everything shipped. SPARK/proof work on the invariants ledger is
  planned, not done (P5-spark).
* **No hand-written JavaScript or TypeScript** (ADR-0003). The UI is
  server-rendered; a test asserts no `<script>` element appears in any
  response. ADR-0007 was ruled on 2026-09-03: *generated* script may ship, but
  only from a named toolchain, reproducibly, and listed in a provenance
  manifest. None exists, so nothing is served — see
  `docs/decisions/0007-client-script-doctrine.adoc`.
* **No async runtime, no web framework** in `berrywiki-serve` — it is a
  single-user localhost tool, and the HTTP surface is the seam to swap later.
* **AsciiDoc** for technical docs and ADRs; Markdown only for wiki content and
  community-health files.

## Honesty about verification

GitHub Wiki behaviours that BerryWiki relies on (HTML-comment invisibility,
`_Sidebar.md` rendering, filename↔title mapping, fine-grained PAT access to
`.wiki.git`) are recorded in `docs/compatibility/github-wiki.adoc`. Rows there
marked unverified **have not been tested against a live wiki**. The live spikes
that would verify them are credential-gated work packages (P1-spike-read,
P3-spike-write). Nothing in this repository should be read as claiming those
behaviours are confirmed.

## Maintainers

Maintained by Jonathan D.A. Jewell (@hyperpolymath). The repository lives in the
`metadatastician` organisation; see `MAINTAINERS`.
