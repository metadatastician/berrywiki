# Contributing to BerryWiki

Thank you for considering a contribution. This document covers how to build the
project, what the non-negotiable constraints are, and how to get a change
merged.

## The constraints that shape every change

BerryWiki is a companion layer for GitHub Wikis, and one principle overrides
everything else:

> **The wiki must stay fully usable when BerryWiki is not.** A user must always
> be able to clone the `.wiki.git` repo, edit the Markdown in any editor,
> commit, push, and read it through GitHub's own Wiki UI.

Concretely, these are treated as bugs rather than preferences. A change that
violates one will not be merged, however useful it otherwise is:

* **No hand-written JavaScript or TypeScript** (ADR-0003). The UI is
  server-rendered; a test asserts no `<script>` element appears in any
  response. *Generated* script is permitted in principle by ADR-0007 (ruled
  2026-09-03) and forbidden in practice until a provenance manifest and its
  served-response gate exist. Neither does, so the no-`<script>` test stands.
* **App state never enters the wiki clone** (ADR-0008). Drafts, journals and
  indexes live under the XDG state home, so a plain-git user cannot commit them.
* **Hierarchy lives in metadata, never in filenames** (ADR-0001).
* **Git safety:** never force-push, never discard local work, fetch before push,
  atomic logical commits, sidebar regenerated with the tree change.
* **Determinism:** metadata serialisation is idempotent and sidebar output is
  byte-deterministic — re-saving an unchanged page must produce no diff.
* **Malformed input degrades with a diagnostic.** It never panics and never
  destroys content. Unknown metadata fields are preserved verbatim.
* **Honesty about verification.** Never describe live GitHub behaviour as
  confirmed unless it was actually tested against a real wiki. See
  `docs/compatibility/github-wiki.adoc`.

## Getting set up

You need **Rust 1.89 or newer** and nothing else — no Node, no database, no
container runtime.

```sh
git clone https://github.com/metadatastician/berrywiki.git
cd berrywiki

cargo build --workspace
cargo test  --workspace
```

To try it against the bundled fixture notebook:

```sh
cargo run -p berrywiki-cli -- serve fixtures/test-wiki
```

## Before you open a pull request

```sh
cargo test --workspace                                    # must pass
cargo build --workspace                                   # must be warning-free
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo test` is the safety gate, not a formality: it carries the script-free SSR
assertions and the git conflict / no-data-loss harness. If you do not have
`rustfmt`/`clippy` locally, CI runs both on every pull request.

## Repository layout

```
crates/berrywiki-core/       deterministic engine, no I/O — the rules live here
crates/berrywiki-store/      WikiStore trait + LocalFolderStore (atomic writes)
crates/berrywiki-appstate/   out-of-clone app state (XDG) + operation journal
crates/berrywiki-draft/      per-page drafts, outside the clone
crates/berrywiki-git/        closed-set git CLI wrapper
crates/berrywiki-sync/       store mutation -> one atomic logical commit
crates/berrywiki-github/     .wiki.git mirror adapter (read-only)
crates/berrywiki-git-compat/ no-data-loss evidence harness
crates/berrywiki-render/     Markdown -> HTML (comrak, raw HTML escaped)
crates/berrywiki-serve/      zero-JS three-pane explorer and editor
crates/berrywiki-cli/        the `berrywiki` command
fixtures/test-wiki/          fixture notebook used by tests
docs/decisions/              ADRs — read these before proposing design changes
docs/execution/              work packages + debt register
```

Work is organised as packages in `docs/execution/work-packages.adoc`; known debt
is in `docs/execution/debt-register.adoc`. Both are good places to find
something to pick up.

## Where changes belong

* Rules with no I/O — parsing, hierarchy, diagnostics, sidebar → `berrywiki-core`,
  with unit tests in the same file.
* Anything that writes wiki content → `berrywiki-store`, which owns atomicity,
  path validation and the stale-write guard. Do not write wiki files elsewhere.
* UI → `berrywiki-serve`. It has **no third-party dependencies**; please keep it
  that way, and add a route to the script-free test sweep.

## Reporting bugs

Use the [bug report template](.github/ISSUE_TEMPLATE/bug_report.yml). Please
include the BerryWiki version or commit, your OS, and — if a wiki file is
involved — a minimal page that reproduces it. A page whose content is private
can usually be reduced to a few lines that still trigger the fault.

If the bug involves **data loss or a page being clobbered**, say so prominently;
that class is treated as urgent.

## Suggesting features

Use the [feature request template](.github/ISSUE_TEMPLATE/feature_request.yml).
Please check `docs/execution/work-packages.adoc` first — the feature may already
be planned, and the package will tell you what is blocking it. If a proposal
touches one of the constraints above, it needs an ADR rather than a PR; open a
discussion first.

## Branch naming and commits

```
feat/short-description        # new capability
fix/short-description         # bug fix
docs/short-description        # documentation only
refactor/what-changed         # no behaviour change
security/what-fixed           # security fix
```

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[body explaining why, not what]
```

Explain *why* in the body. The what is visible in the diff; the reasoning is
not, and it is what a future maintainer needs.

## Security

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md)
for private reporting.

## Licence

By contributing you agree that your code is licensed under **MPL-2.0** and your
documentation under **CC-BY-SA-4.0**, matching the rest of the project.
