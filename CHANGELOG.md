<!--
SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# Changelog

All notable changes to this project are recorded here, in the spirit of
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**There is no released version yet.** The repository carries no tags and no
releases, so there is no version heading below and no comparison links — adding
either would assert a release that does not exist. The first version heading
appears when `v0.1.0` is cut, which is package `P8-packaging` and is blocked on
`P5-guix` under ruling R-C.

Under ruling D-12(b) that release ships as one packaging system with two
artefacts — a Guix package, and a signed binary (`SHA256SUMS` + a `minisign`
signature) alongside the OCI image produced by `guix pack`. Work package
`P5-guix` is the gate on all three. Zero tags and zero releases exist on
`origin` as of 2026-09-15.

## [Unreleased]

### Added

- **A `berrywiki check` composite Action** at the repository root
  (`action.yml`), so a course wiki can be structure-checked in GitHub Actions
  with `uses: metadatastician/berrywiki@<sha>`. It builds the CLI from the
  pinned revision and caches the binary against `github.action_ref`, so callers
  pinned to a commit restore a cached binary instead of rebuilding. Inputs are
  passed to the shell through `env:` rather than interpolated into `run:`
  bodies, which is what keeps a workflow input from being executed as script.
- **A course wiki template repository**,
  [`metadatastician/berrywiki-course-template`](https://github.com/metadatastician/berrywiki-course-template)
  — 24 pre-structured stub pages under `wiki/`, CC-BY-SA-4.0, public and
  marked as a template from 2026-09-15. Under ruling D-11(c) the template is
  the delivery: a teacher presses *Use this template* and installs nothing.
- **Community drafts** under `docs/community/`, including the GitHub Global
  Campus Teachers post. These are owner-posted, never agent-posted (D-9).
- Read-only SSR explorer over a GitHub Wiki clone, zero-JS by doctrine.
- In-app editing with drafts held outside the clone, and in-app sync
  (commit / pull / push) with conflict classification.
- Tags, per-page history from real `git log`, and attachments stored at
  `assets/<page-id>/<filename>`.
- `berrywiki check`: validates a wiki and exits non-zero on error-level
  diagnostics (the composite Action above builds on this verb).
- `berrywiki import`: CherryTree notebook import with a lossiness table and a
  per-construct diagnostic tally printed before `--apply` writes anything.
- An accessibility gate in CI.
- **A `pins` job in CI** (`action pins resolve`) that asks the GitHub API
  whether every SHA-pinned `uses:` in `.github/workflows/`, `.github/actions/`
  and the root `action.yml` names a commit that exists, and — for
  reusable-workflow pins only — whether that commit is reachable from the
  callee's default branch. A determinate negative fails the job; an
  indeterminate answer is reported as UNVERIFIED rather than failed, so a
  GitHub incident cannot redden every run. It closes the class of bug that
  produced the `pages.yml` pin under *Fixed* below.

### Changed

- **Licensing is split and machine-detectable**: MPL-2.0 for code,
  CC-BY-SA-4.0 for prose. The root `LICENSE` is now a verbatim MPL-2.0 text,
  because a prepended preamble lowers GitHub's licence-detection confidence
  far enough that the repository reports its licence as `other`.
- Documentation that described an eleven-crate system now says thirteen, which
  is what `crates/` contains.
- **Every action pin now carries the exact release it is** — `checkout@v7.0.1`
  and `deploy-pages@v5.0.1`, not the `# v7` / `# v5` they said — and the two
  refs Dependabot cannot move were brought up to date: the scorecard reusable
  `a63b2761` → `7b931ef7` (it no longer lets a failed reconciler skip the SARIF
  upload, and it requires `actions: read`, which this caller already granted)
  and the secret scanner `c51fb976` → `e13e2ea3` (it fetches its own config at
  `job.workflow_sha` instead of the moving `main`, and adds a gating
  full-history pass). Each pin's comment records why the bump is safe, and
  `docs/development/ci.adoc` now writes down the bump rule — a reusable pin
  targets a commit with no tag, so it has no version for Dependabot to compare
  and nothing was watching it.
- `docs/development/ci.adoc` said `actions/checkout@v5` and
  `Swatinem/rust-cache@v2` long after the pins had moved, and still listed
  "SHA-pin actions once the action-trust-layers policy is applied estate-wide"
  as a follow-up. Both corrected, along with a statement of what Dependabot
  does and does not keep current here.

### Fixed

- **`pages.yml` pinned `actions/upload-pages-artifact` to a commit that exists
  nowhere.** `3788795898…` answers 422 from the commits endpoint; tag `v5` is
  `fc324d354710…`. A pin naming no commit is still forty hex characters, so
  nothing local could catch it, and GitHub resolves a `uses:` ref only at run
  time — an unresolvable one emits no check run at all, so the job never ran
  while the board stayed green. Third occurrence of the class in this
  repository (two in `ci.yml`, cleared 2026-08-07), which is why the `pins` job
  above now exists.

### Removed

- The desktop launcher pair and its CI step (ruling R-F).

### Notes on what is *not* claimed

- **MSRV 1.89 is verified in CI**, by the `test (rust 1.89.0)` matrix lane,
  green on `0cf0066`. Local host checks run on rustc 1.97.1, so a green local
  run is not by itself an MSRV result — the CI lane is.
- The test baseline at `0cf0066` is `cargo test --workspace` exit 0 — 42
  suites, 429 passed, 0 failed, 0 ignored. It was produced against a warm
  `target/`, so it is not a cold-build result.
- The accessibility walkthrough (`docs/execution/a11y-walkthrough.adoc`) is
  written and **has never been executed**; it needs a human with a screen
  reader. The structural gate is real; the walkthrough half is not claimed.
  It is declared unrun in the README and in the published wiki.
- `P1-spike-read` has never run, so every GitHub Wiki behaviour the project
  relies on is reasoned from documentation rather than tested against a real
  wiki.
- GitHub reports the licence as `other`: a six-line preamble precedes the
  MPL-2.0 text in `LICENSE`, which drops GitHub's detector below its match
  threshold. `REUSE.toml` and `LICENSES/` are correct and remain normative.
- **Dependabot does not raise routine Cargo bumps here, on purpose.** The cargo
  block is `open-pull-requests-limit: 0` (security updates only), because
  `ignore` rules apply to security updates too and that mistake silenced
  patch-level security PRs estate-wide. As of 2026-09-25 the one bump this
  leaves outstanding is `comrak` 0.54.0 → 0.55.0; it has **not** been applied
  here, since `Cargo.lock` freezes the rustc-1.89-compatible resolution and
  nothing in this change set was built against a Rust toolchain. Raising the
  limit to `2` is the one-line way to have Dependabot propose it.

[Unreleased]: https://github.com/metadatastician/berrywiki/commits/main
