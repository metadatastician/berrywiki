## What this changes

<!-- One paragraph. What does the program do after this that it did not before?
     If the answer is "nothing", say so and explain what the change is for. -->

## Why

<!-- Link the work package from docs/execution/work-packages.adoc, the ADR, or
     the issue. A change with no tracked package needs a sentence saying why it
     is not one. -->

## How it was verified

<!-- Name what you RAN, not what you believe. "cargo test --workspace --locked:
     429 passed, 0 failed" is a verification; "tests should pass" is not.

     If a check could not be run here, say which and why — an honest gap is
     worth more than an unearned claim. -->

- [ ] `cargo test --workspace --locked` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --check` is clean
- [ ] `scripts/check-adoc.sh` passes (if any `.adoc` changed)
- [ ] `berrywiki check` passes on the published wiki (if `wiki/` changed)

## Doctrine checklist

- [ ] **Zero-JS**: no client-side script. Behaviour lives behind HTTP endpoints.
- [ ] **Accessibility**: new UI is reachable and operable without a pointer, and
      anything asserted about a screen reader was actually observed with one.
- [ ] **Licensing**: new files are covered by `REUSE.toml` or carry an SPDX
      header. Changes to `LICENSE`, `LICENSES/` or licence terms are an owner
      decision and are not made here.
- [ ] **No new dependency** — or, if there is one, it is named here with the
      reason, because every added crate is also a packaging cost under
      `P5-guix`.
- [ ] Documentation that describes changed behaviour was updated in the same PR.

## Anything a reviewer should look at first

<!-- Point at the part you are least sure about. -->
