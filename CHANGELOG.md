# Changelog

All notable changes to BerryWiki are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Dates are ISO-8601. Entries describe behaviour a user can observe; internal
refactors appear only where they change what the program does.

## [Unreleased]

No release has been cut yet. `origin` carries zero tags and zero releases, so
every entry below is unreleased by construction, and the first tagged version
will be `v0.1.0`.

Under ruling D-12(b) that release ships as one packaging system with two
artefacts — a Guix package, and a signed binary (`SHA256SUMS` + a `minisign`
signature) alongside the OCI image produced by `guix pack`. Work package
`P5-guix` is the gate on all three.

### Added

- Read-only SSR explorer over a GitHub Wiki clone, zero-JS by doctrine.
- In-app editing with drafts held outside the clone, and in-app sync
  (commit / pull / push) with conflict classification.
- Tags, per-page history from real `git log`, and attachments stored at
  `assets/<page-id>/<filename>`.
- `berrywiki check`: validates a wiki and exits non-zero on error-level
  diagnostics.
- `berrywiki import`: CherryTree notebook import with a lossiness table and a
  per-construct diagnostic tally printed before `--apply` writes anything.
- An accessibility gate in CI.

### Known gaps

- The accessibility walkthrough has never been run by a human with a screen
  reader. It is declared unrun in the README and in the published wiki.
- `P1-spike-read` has never run, so every GitHub Wiki behaviour the project
  relies on is reasoned from documentation rather than tested against a real
  wiki.
- GitHub reports the licence as `other`: a six-line preamble precedes the
  MPL-2.0 text in `LICENSE`, which drops GitHub's detector below its match
  threshold. `REUSE.toml` and `LICENSES/` are correct and remain normative.

[Unreleased]: https://github.com/metadatastician/berrywiki/commits/main
