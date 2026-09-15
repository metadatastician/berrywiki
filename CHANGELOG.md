<!--
SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
SPDX-License-Identifier: CC-BY-SA-4.0
-->

# Changelog

All notable changes to this project are recorded here, in the spirit of
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

**There is no released version yet.** The repository carries no tags and no
releases, so there is no version heading below and no comparison links — adding
either would assert a release that does not exist. The first version heading
appears when `v0.1.0` is cut, which is package `P8-packaging` and is blocked on
`P5-guix` under ruling R-C.

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

### Changed

- **Licensing is split and machine-detectable**: MPL-2.0 for code,
  CC-BY-SA-4.0 for prose. The root `LICENSE` is now a verbatim MPL-2.0 text,
  because a prepended preamble lowers GitHub's licence-detection confidence
  far enough that the repository reports its licence as `other`.
- Documentation that described an eleven-crate system now says thirteen, which
  is what `crates/` contains.

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
