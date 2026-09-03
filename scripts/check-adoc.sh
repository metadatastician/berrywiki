#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
# SPDX-License-Identifier: MPL-2.0
#
# check-adoc.sh
#
# Render gate for every tracked .adoc file.
#
# Why this is not `asciidoctor "$f"` under `set -e`
# -------------------------------------------------
# asciidoctor's default failure level is FATAL. A malformed table emits
#     WARNING: dropping cells from incomplete row
# or
#     ERROR: table missing leading separator; recovering automatically
# on stderr and **exits 0**. The content is silently wrong: a literal `|` in
# prose inside a cell shifts every later cell, so a row lands perfectly formed
# in the wrong column. `set -euo pipefail` gives no protection against a class
# of message literally labelled ERROR, because the exit code never carries it.
#
# So the verdict here is not the exit code alone. A file is clean only when
# asciidoctor exits 0 AND writes nothing to stderr:
#
#     exit 0, stderr empty   -> clean
#     exit 0, stderr non-empty -> finding   (the trap this gate exists for)
#     exit non-zero            -> finding, or the run did not complete
#
# A fixture trap, recorded because it caught the author of this script: a
# document title must be followed by a BLANK LINE before `[cols=...]`, or the
# attribute line is swallowed by the document header, the table never opens,
# and the file fails for a reason that has nothing to do with its cells.
#
# `--failure-level=WARN` is passed as well, so the exit code agrees with the
# stderr check rather than contradicting it; the stderr check is what catches
# anything asciidoctor writes outside its logger. This is the three-way
# contract `empty-linter` already ships (0 clean / 1 finding / 2 did not
# complete) and that asciidoctor collapses by default.
#
# Why the gate tests itself first
# -------------------------------
# A gate that has never been observed to fail is not evidence of anything. So
# before the real files are scanned, this script renders a planted malformed
# table and requires it to be reported, and a planted well-formed file and
# requires it to pass. Either self-test failing is a hard failure: it means the
# gate can no longer tell the two apart, which is worse than no gate.
#
# Fail-closed: asciidoctor missing is a FAILURE, never a skip. An availability
# probe that silently skips and reports success is how a gate becomes
# permanently, invisibly green.
#
# Usage: scripts/check-adoc.sh          (from the repo root)
#   CHECK_ADOC_NO_ASCIIDOCTOR=1         explicit opt-out, warns loudly
#   ASCIIDOCTOR=<path>                  use a specific binary
set -euo pipefail

fail() { printf 'adoc gate: FAIL: %s\n' "$*" >&2; exit 1; }

# --- 0. the tool itself, fail-closed ------------------------------------

adoc="${ASCIIDOCTOR:-}"
if [ -z "$adoc" ] && command -v asciidoctor >/dev/null 2>&1; then
    adoc="$(command -v asciidoctor)"
fi
if [ -z "$adoc" ]; then
    if [ "${CHECK_ADOC_NO_ASCIIDOCTOR:-}" = "1" ]; then
        printf 'adoc gate: WARNING: asciidoctor not found, skipped by request\n' >&2
        exit 0
    fi
    fail "asciidoctor not found (set ASCIIDOCTOR=<path>, or CHECK_ADOC_NO_ASCIIDOCTOR=1 to skip)"
fi

# Recorded because the gate is a stderr comparison: a newer asciidoctor can
# emit a diagnostic an older one did not, so a version change is the first
# thing to check when CI reddens on a file the host renders clean.
printf 'adoc gate: using %s (%s)\n' "$adoc" "$("$adoc" --version | head -1)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# render FILE -> prints any diagnostic to stdout, returns 1 when unclean.
render() {
    local f="$1" err rc
    set +e
    err="$("$adoc" --failure-level=WARN --backend=html5 -o /dev/null "$f" 2>&1 >/dev/null)"
    rc=$?
    set -e
    if [ "$rc" -eq 0 ] && [ -z "$err" ]; then
        return 0
    fi
    printf '%s\n' "${err:-(no diagnostic; exit $rc)}"
    return 1
}

# --- 1. prove the gate can fail -----------------------------------------
#
# A stray `|` in prose inside a cell. This is the real defect shape: it was
# found in docs/execution/work-packages.adoc on 2026-09-03, where asciidoctor
# reported it and exited 0.

cat > "$tmp/known-bad.adoc" <<'BAD'
= Planted malformed table

[cols="2,1"]
|===
| Property | Assertion

| A cell with a bare | pipe in prose | shifts every later cell
|===
BAD

cat > "$tmp/known-good.adoc" <<'GOOD'
= Planted well-formed document

[cols="2,1"]
|===
| Property | Assertion

| A cell with an escaped \| pipe | renders as one cell
|===
GOOD

if render "$tmp/known-bad.adoc" >/dev/null; then
    fail "self-test: the planted malformed table was reported CLEAN. The gate cannot fail, so it proves nothing about the real files."
fi
printf 'adoc gate: self-test: planted malformed table correctly reported\n'

if ! bad_out="$(render "$tmp/known-good.adoc")"; then
    fail "self-test: the planted well-formed document was reported unclean, so the gate cannot tell good from bad: $bad_out"
fi
printf 'adoc gate: self-test: planted well-formed document correctly passed\n'

# --- 2. the real files ---------------------------------------------------

files="$(git ls-files '*.adoc')"
n_files=$(printf '%s\n' "$files" | grep -c . || true)
# An empty file list is a defect, not a pass: this repository documents itself
# in AsciiDoc, so zero tracked .adoc files means the extraction broke.
[ "$n_files" -gt 0 ] || fail "no tracked .adoc files found (extraction broken?)"

unclean=0
while read -r f; do
    [ -n "$f" ] || continue
    if ! out="$(render "$f")"; then
        printf 'adoc gate: %s\n' "$f" >&2
        printf '%s\n' "$out" | sed 's/^/  /' >&2
        unclean=$((unclean + 1))
    fi
done <<< "$files"

[ "$unclean" -eq 0 ] || fail "$unclean of $n_files .adoc file(s) did not render cleanly"
printf 'adoc gate: OK: %s tracked .adoc file(s) render with no diagnostic\n' "$n_files"
