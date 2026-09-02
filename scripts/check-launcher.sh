#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jonathan D.A. Jewell
# SPDX-License-Identifier: MPL-2.0
#
# check-launcher.sh
#
# Smoke gate for berrywiki-launcher.sh. It needs no built binary: a fake
# `berrywiki` on $PATH records the arguments it was started with, and the
# launcher is run from a copy OUTSIDE the checkout so a local target/ build
# cannot leak into the result. Every assertion names the exact text it
# expects; "exits non-zero" alone would be a fake gate.
#
# Checks, in order:
#   1. bash -n and shellcheck on the launcher and every scripts/*.sh
#      (shellcheck absent = FAIL unless CHECK_LAUNCHER_NO_SHELLCHECK=1;
#      SHELLCHECK=<path> overrides the lookup);
#   2. --help exits 0 and documents BERRYWIKI_WIKI;
#   3. --start with BERRYWIKI_WIKI unset exits non-zero, says so, writes
#      no PID file;
#   4. --start with a folder and a fake binary runs `berrywiki serve <dir>`
#      exactly, reports the URL, --status says Running, --stop stops it and
#      --status then says Not running.
#
# Usage: scripts/check-launcher.sh          (from anywhere)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
launcher="$PWD/berrywiki-launcher.sh"

fail() { printf 'check-launcher: FAIL: %s\n' "$*" >&2; exit 1; }
ok()   { printf 'check-launcher: ok: %s\n' "$*"; }

# --- 1. syntax + shellcheck ---------------------------------------------
scripts=("$launcher")
for s in scripts/*.sh; do scripts+=("$PWD/$s"); done
for s in "${scripts[@]}"; do
    bash -n "$s" || fail "bash -n $s"
done
ok "bash -n on ${#scripts[@]} scripts"

shellcheck_bin="${SHELLCHECK:-}"
if [ -z "$shellcheck_bin" ] && command -v shellcheck >/dev/null 2>&1; then
    shellcheck_bin="$(command -v shellcheck)"
fi
if [ -n "$shellcheck_bin" ]; then
    "$shellcheck_bin" -S style "${scripts[@]}" || fail "shellcheck"
    ok "shellcheck ($("$shellcheck_bin" --version | sed -n 's/^version: //p'))"
elif [ "${CHECK_LAUNCHER_NO_SHELLCHECK:-0}" = "1" ]; then
    printf 'check-launcher: WARNING: shellcheck not found, skipped by request\n' >&2
else
    fail "shellcheck not found (set SHELLCHECK=<path>, or CHECK_LAUNCHER_NO_SHELLCHECK=1 to skip)"
fi

# --- sandbox --------------------------------------------------------------
tmp="$(mktemp -d)"
cleanup() {
    if [ -f "$tmp/run/berrywiki.pid" ]; then
        kill "$(cat "$tmp/run/berrywiki.pid")" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT

mkdir -p "$tmp/run" "$tmp/state" "$tmp/home" "$tmp/bin" "$tmp/wiki" "$tmp/outside"
export XDG_RUNTIME_DIR="$tmp/run"
export XDG_STATE_HOME="$tmp/state"
export HOME="$tmp/home"
unset BERRYWIKI_WIKI BERRYWIKI_REPO_DIR DISPLAY WAYLAND_DISPLAY
# A copy outside the checkout: REPO_DIR must resolve to empty, so the only
# binary the launcher can find is the fake one on $PATH.
cp "$launcher" "$tmp/outside/berrywiki-launcher.sh"
L="$tmp/outside/berrywiki-launcher.sh"
pid_file="$tmp/run/berrywiki.pid"
args_file="$tmp/args"

cat > "$tmp/bin/berrywiki" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$@" > "$FAKE_ARGS"
exec sleep 60
FAKE
chmod +x "$tmp/bin/berrywiki"
export FAKE_ARGS="$args_file"

# --- 2. --help --------------------------------------------------------------
help_out="$(bash "$L" --help)" || fail "--help exited non-zero"
grep -q -- '--start' <<<"$help_out" || fail "--help does not list --start"
grep -q 'BERRYWIKI_WIKI' <<<"$help_out" || fail "--help does not document BERRYWIKI_WIKI"
ok "--help"

# --- 3. refuse without a wiki folder ----------------------------------------
set +e
err_out="$(PATH="$tmp/bin:$PATH" bash "$L" --start 2>&1 >/dev/null)"
rc=$?
set -e
[ "$rc" -ne 0 ] || fail "--start with BERRYWIKI_WIKI unset exited 0"
grep -q 'No wiki folder set' <<<"$err_out" || fail "expected 'No wiki folder set', got: $err_out"
grep -q 'BERRYWIKI_WIKI' <<<"$err_out" || fail "refusal does not name BERRYWIKI_WIKI"
[ ! -e "$pid_file" ] || fail "PID file written on refusal"
[ ! -e "$args_file" ] || fail "binary was started on refusal"
ok "--start refuses without BERRYWIKI_WIKI (exit $rc, no PID file)"

# --- 4. start / status / stop with the fake binary ---------------------------
start_out="$(PATH="$tmp/bin:$PATH" BERRYWIKI_WIKI="$tmp/wiki" bash "$L" --start 2>&1)" \
    || fail "--start with a wiki folder failed: $start_out"
[ -f "$pid_file" ] || fail "no PID file after --start"
kill -0 "$(cat "$pid_file")" 2>/dev/null || fail "PID in $pid_file is not alive"
[ -f "$args_file" ] || fail "fake binary was not started"
expected_args="$(printf 'serve\n%s\n' "$tmp/wiki")"
[ "$(cat "$args_file")" = "$expected_args" ] \
    || fail "binary argv mismatch: got '$(tr '\n' ' ' <"$args_file")', want 'serve $tmp/wiki'"
grep -q 'http://127.0.0.1:23779' <<<"$start_out" || fail "--start did not report the URL"
ok "--start runs 'berrywiki serve <dir>' (PID $(cat "$pid_file"))"

status_out="$(bash "$L" --status 2>&1)"
grep -q '^.*Running (PID' <<<"$status_out" || fail "--status while running: $status_out"
ok "--status reports Running"

bash "$L" --stop >/dev/null 2>&1 || fail "--stop failed"
[ ! -e "$pid_file" ] || fail "PID file survives --stop"
status_out="$(bash "$L" --status 2>&1)"
grep -q 'Not running' <<<"$status_out" || fail "--status after stop: $status_out"
ok "--stop, then --status reports Not running"

printf 'check-launcher: PASS\n'
