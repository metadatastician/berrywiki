#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
# SPDX-License-Identifier: MPL-2.0
#
# check-invariants-ledger.sh [TEST_LIST]
#
# Gate for docs/proofs/invariants.adoc. Every witness row in the ledger has
# the exact shape
#     | `crates/<crate>/<path>.rs` | `<test_fn>`
# and this script fails (exit 1) unless, for each row, the file exists and
# declares `fn <test_fn>(`, and, when TEST_LIST is given, `<test_fn>` appears
# as a registered test in that file (the saved output of
# `cargo test --workspace -- --list`). It also fails when the ledger has no
# witness rows, when any INV section has none, when the number of INV
# sections is not six, or when TEST_LIST lists no tests at all. It is
# fail-closed on purpose: an empty extraction is a defect, not a pass.
set -euo pipefail

ledger="${BERRYWIKI_LEDGER:-docs/proofs/invariants.adoc}"
test_list="${1:-}"

fail() { printf 'invariants ledger: FAIL: %s\n' "$*" >&2; exit 1; }

[ -f "$ledger" ] || fail "ledger not found at $ledger"

# One ERE, written without backslash escapes so grep, awk and sed all read it
# identically: [|] is a literal pipe, [.] a literal dot.
# shellcheck disable=SC2016  # the $ is a regex anchor, not a variable
row_re='^[|] `(crates/[A-Za-z0-9_./-]+[.]rs)` [|] `([A-Za-z0-9_]+)`$'

# Section coverage: every "== INV-n" heading must own at least one witness row.
sections=$(ROW_RE="$row_re" awk '
  BEGIN            { re = ENVIRON["ROW_RE"] }
  /^== INV-[0-9]+/ { if (name != "") print name, count; name = $2; count = 0; next }
  $0 ~ re          { count++ }
  END              { if (name != "") print name, count }
' "$ledger")
n_sections=$(printf '%s\n' "$sections" | grep -c . || true)
[ "$n_sections" -eq 6 ] || fail "expected 6 INV sections, found $n_sections"
empty=$(printf '%s\n' "$sections" | awk '$2 == 0 { print $1 }')
[ -z "$empty" ] || fail "INV section(s) with no witness row: $(printf '%s' "$empty" | tr '\n' ' ')"

# Witness extraction: "<file> <test_fn>" per row.
rows=$(grep -E "$row_re" "$ledger" | sed -E "s#$row_re#\1 \2#")
n_rows=$(printf '%s\n' "$rows" | grep -c . || true)
[ "$n_rows" -gt 0 ] || fail "no witness rows extracted (row shape changed?)"

n_registered=0
if [ -n "$test_list" ]; then
  [ -f "$test_list" ] || fail "test list not found at $test_list"
  n_registered=$(grep -cE ': test$' "$test_list" || true)
  [ "$n_registered" -gt 0 ] || fail "test list $test_list names no tests"
fi

missing=0
while read -r file name; do
  if [ ! -f "$file" ]; then
    printf 'invariants ledger: missing file %s (witness %s)\n' "$file" "$name" >&2
    missing=$((missing + 1)); continue
  fi
  if ! grep -qE "^[[:space:]]*(pub )?fn ${name}\(" "$file"; then
    printf 'invariants ledger: %s does not declare fn %s\n' "$file" "$name" >&2
    missing=$((missing + 1)); continue
  fi
  if [ -n "$test_list" ] && ! grep -qE "(^|::)${name}: test$" "$test_list"; then
    printf 'invariants ledger: %s is declared in %s but not registered as a test\n' "$name" "$file" >&2
    missing=$((missing + 1))
  fi
done <<< "$rows"

[ "$missing" -eq 0 ] || fail "$missing of $n_rows witness rows unsatisfied"
mode="declared in source"
[ -n "$test_list" ] && mode="declared and registered (test list: $n_registered tests)"
printf 'invariants ledger: OK: %s witness rows across %s invariants, all %s\n' "$n_rows" "$n_sections" "$mode"
