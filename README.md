# BerryWiki

A companion authoring, navigation, indexing and synchronisation layer for
**GitHub.com Wikis** that gives them a CherryTree/Zim-style hierarchical
notebook experience — while keeping the content as plain, GitHub-compatible
Markdown in ordinary Git storage.

> **Core principle:** the wiki must stay fully usable when BerryWiki is not.
> You can always clone the `.wiki.git` repo, read/edit the Markdown in any
> editor, commit, push, and view it through GitHub's normal Wiki UI.
> BerryWiki *enhances* the files; it never makes them depend on the app.

## Status — Phase 0 (Compatibility & Authentication Spike)

This repository is at **Phase 0**: proving the vertical path and building the
deterministic core engine. No production GitHub synchronisation and no rich
editor yet. See [`docs/architecture/phase-0-plan.adoc`](docs/architecture/phase-0-plan.adoc).

Implemented so far:

- `berrywiki-core` — deterministic, I/O-free engine:
  - hidden BerryWiki metadata block parse/serialise (round-trip stable),
  - heading + wiki-link/Markdown-link extraction,
  - page hierarchy from stable ids, sibling ordering, backlinks,
  - consistency diagnostics (broken links, missing parent, cycles, duplicate ids),
  - deterministic `_Sidebar.md` generation.
- `fixtures/test-wiki/` — a small, human-readable fixture notebook.

## Technology

- **Engine & tooling: Rust** (estate default *Rust/GNATprove*;
  mathematically sound base).
- **Web UI: typed-wasm**, deferred until the language reaches base-language completion.
- **Docs:** AsciiDoc (`.adoc`) for technical docs and ADRs; Markdown (`.md`) for wiki content and community-health files.

## Layout

```
crates/berrywiki-core/   deterministic engine (no I/O)
fixtures/test-wiki/       fixture notebook (Markdown)
docs/architecture/        plan + overview
docs/compatibility/       GitHub Wiki compatibility findings
docs/decisions/           architecture decision records
```

## Build & test

```sh
cargo test        # unit tests for the core engine
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Licence

Dual-licensed under MPL-2.0 for code and CC-BY-SA-4.0 for docs.
