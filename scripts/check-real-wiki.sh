#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
# SPDX-License-Identifier: MPL-2.0
#
# check-real-wiki.sh BERRYWIKI_BIN WIKI_DIR EXPECTED_PAGES
#
# Evidence gate for v1 criterion R4: BerryWiki is in use on a wiki it did not
# create, and `berrywiki check` passes there. WIKI_DIR is that consumer's wiki
# folder, checked out at a pinned commit by the caller.
#
# It fails (exit 1) unless all of these hold:
#   1. `berrywiki check WIKI_DIR` exits 0 and reports exactly EXPECTED_PAGES
#      pages, 0 errors and 0 warnings;
#   2. `berrywiki sidebar WIKI_DIR` reproduces the committed _Sidebar.md byte
#      for byte;
#   3. the planted negative is rejected: a scratch copy in which one page takes
#      another page's id and gains a link to a missing page makes
#      `berrywiki check` exit 1 with at least one error.
#
# Step 3 is what lets steps 1 and 2 count as evidence: a check that cannot
# fail on this tree would pass it whatever the tree contained. The mutation is
# itself verified to have changed the copy, so a no-op mutant fails the gate
# rather than passing it vacuously.
set -euo pipefail

bin="${1:?usage: check-real-wiki.sh BERRYWIKI_BIN WIKI_DIR EXPECTED_PAGES}"
wiki="${2:?usage: check-real-wiki.sh BERRYWIKI_BIN WIKI_DIR EXPECTED_PAGES}"
expected_pages="${3:?usage: check-real-wiki.sh BERRYWIKI_BIN WIKI_DIR EXPECTED_PAGES}"

# fail MESSAGE...: print a gate failure and exit 1.
fail() { printf 'real-wiki check: FAIL: %s\n' "$*" >&2; exit 1; }

# run_check DIR: run `berrywiki check DIR`, print its output, and set the
# globals rc, pages, errors and warnings. Fails the gate when the summary line
# is missing, because a check whose result cannot be found must not pass.
run_check() {
  local out summary
  set +e
  out="$("$bin" check "$1" 2>&1)"
  rc=$?
  set -e
  printf '%s\n' "$out"
  summary="$(printf '%s\n' "$out" \
    | grep -E '^[0-9]+ error\(s\), [0-9]+ warning\(s\)$' | tail -1 || true)"
  [ -n "$summary" ] || fail "no 'N error(s), M warning(s)' line from check on $1"
  errors="$(printf '%s' "$summary" | awk '{print $1}')"
  warnings="$(printf '%s' "$summary" | awk '{print $3}')"
  pages="$(printf '%s\n' "$out" | sed -n 's/^\([0-9]\+\) page(s) in .*/\1/p' | head -1)"
  [ -n "$pages" ] || fail "no 'N page(s) in' line from check on $1"
}

[ -x "$bin" ] || fail "not an executable: $bin"
[ -d "$wiki" ] || fail "not a directory: $wiki"
[ -f "$wiki/_Sidebar.md" ] || fail "no committed _Sidebar.md in $wiki"

echo "== 1. berrywiki check on the consumer wiki"
run_check "$wiki"
printf 'rc=%s pages=%s errors=%s warnings=%s\n' "$rc" "$pages" "$errors" "$warnings"
[ "$rc" -eq 0 ] || fail "check exited $rc, expected 0"
[ "$pages" = "$expected_pages" ] || fail "expected $expected_pages pages, found $pages"
[ "$errors" = 0 ] || fail "expected 0 errors, found $errors"
[ "$warnings" = 0 ] || fail "expected 0 warnings, found $warnings"

echo "== 2. generated sidebar against the committed _Sidebar.md"
generated="$(mktemp)"
scratch="$(mktemp -d)"
trap 'rm -rf "$generated" "$scratch"' EXIT
"$bin" sidebar "$wiki" > "$generated"
cmp "$generated" "$wiki/_Sidebar.md" \
  || { diff -u "$wiki/_Sidebar.md" "$generated" || true; fail "generated sidebar differs from the committed one"; }
echo "sidebar: byte-identical"

echo "== 3. planted negative: duplicated id plus a link to a missing page"
cp -R "$wiki/." "$scratch/"
mapfile -t md < <(find "$scratch" -maxdepth 1 -name '*.md' ! -name '_*' | LC_ALL=C sort)
[ "${#md[@]}" -ge 2 ] || fail "need at least two pages to plant a duplicate id"
donor_id="$(sed -n 's/^id: \(.*\)$/\1/p' "${md[0]}" | head -1)"
[ -n "$donor_id" ] || fail "no id: line in ${md[0]##*/}"
before="$(cat "${md[1]}")"
sed -i "0,/^id: .*/s//id: ${donor_id}/" "${md[1]}"
printf '\nSee [nowhere](BerryWiki-Planted-Missing-Page).\n' >> "${md[1]}"
[ "$(cat "${md[1]}")" != "$before" ] || fail "the planted mutation did not change ${md[1]##*/}"
grep -q "^id: ${donor_id}\$" "${md[1]}" || fail "the duplicate id was not planted in ${md[1]##*/}"
run_check "$scratch"
printf 'rc=%s pages=%s errors=%s warnings=%s\n' "$rc" "$pages" "$errors" "$warnings"
[ "$rc" -eq 1 ] || fail "planted negative: check exited $rc, expected 1"
[ "$errors" -gt 0 ] || fail "planted negative: check reported 0 errors"

echo "real-wiki check: PASS (the consumer wiki passed and its planted negative was rejected)"
