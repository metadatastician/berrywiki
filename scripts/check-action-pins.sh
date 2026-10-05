#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
#
# check-action-pins.sh -- every SHA-pinned `uses:` must resolve upstream.
#
# Run by the `pins` job in .github/workflows/ci.yml. Moved out of the workflow
# when the workflows became KYAML: KYAML writes a block scalar as one
# double-quoted string, which turned this script into a single escaped line
# that neither a reader nor shellcheck could use. The rationale comment above
# the `pins` job in ci.yml still applies.
#
# Needs GH_TOKEN (or GITHUB_TOKEN) for the API; exit 1 on a determinate
# negative, 0 otherwise (indeterminate answers are reported as UNVERIFIED).
set -uo pipefail

# 1. Collect every SHA-pinned external `uses:` in the workflow directory and in
#    the root composite Action. Local refs (`./…`) and containers are not
#    matched: they carry no upstream commit. Only paths that exist are
#    searched, and grep's stderr is kept: under `bash -e` with pipefail a
#    missing path (grep exit 2) killed this step with no output at all.
#    Exit 1 (no match) is the empty case below, not an error. The key and
#    value may be quoted, as KYAML writes them (`uses: "owner/repo@sha"`);
#    an unquoted-only pattern silently skips every KYAML workflow.
targets=()
for p in .github/workflows .github/actions action.yml; do
  [ -e "$p" ] && targets+=("$p")
done
pins="$({ grep -rhoE '"?uses"?:[[:space:]]*"?[A-Za-z0-9_.-]+/[A-Za-z0-9_./-]+@[0-9a-f]{40}' \
            "${targets[@]}" || [ $? -eq 1 ]; } \
        | sed -E 's/^"?uses"?:[[:space:]]*"?//' \
        | awk -F'@' '{ split($1, p, "/"); k = ($1 ~ /\.github\/workflows\/[^\/]+\.ya?ml$/) ? "R" : "A";
                       print p[1] "/" p[2] "\t" $2 "\t" k }' \
        | sort -u)"

if [ -z "$pins" ]; then
  echo "no SHA-pinned external uses: found — nothing to check"
  exit 0
fi

count="$(printf '%s\n' "$pins" | wc -l | tr -d ' ')"
echo "checking $count unique pin(s) against the GitHub API"
echo

api() { # api <path> — sets HTTP_CODE and API_BODY
  local path="$1" auth=() out
  [ -n "${GH_TOKEN:-${GITHUB_TOKEN:-}}" ] && \
    auth=(-H "Authorization: Bearer ${GH_TOKEN:-$GITHUB_TOKEN}")
  out="$(curl -sS -w $'\n%{http_code}' --max-time 30 \
    -H 'Accept: application/vnd.github+json' \
    ${auth[@]+"${auth[@]}"} \
    "https://api.github.com/$path")" || out="{}"$'\n'000
  HTTP_CODE="${out##*$'\n'}"
  API_BODY="${out%$'\n'*}"
}

failed=0
unverified=0
declare -A default_branch=()

while IFS=$'\t' read -r spec sha kind; do
  [ -n "$spec" ] || continue
  repo="$(printf '%s' "$spec" | cut -d/ -f1,2)"
  short="$(printf '%s' "$sha" | cut -c1-12)"

  # 2. Existence. A determinate negative fails: GitHub answered that no such
  #    commit is in that repository.
  api "repos/$repo/commits/$sha"
  case "$HTTP_CODE" in
    200) ;;
    404 | 422)
      echo "  FAIL  $spec@$short… — no such commit in $repo (HTTP $HTTP_CODE)"
      failed=$((failed + 1))
      continue
      ;;
    *)
      echo "  UNVERIFIED  $spec@$short… — API answered HTTP $HTTP_CODE"
      unverified=$((unverified + 1))
      continue
      ;;
  esac

  # 3. Reachability, for reusable-workflow pins only. GitHub resolves a called
  #    workflow only at a commit reachable from the callee's default branch, so
  #    an orphaned commit object — a squashed-away branch, a deleted unmerged
  #    branch — answers 200 above and still fails at graph resolution, with
  #    zero jobs and no check run. Ordinary action pins are exempt: they are
  #    fetched by object id, where a non-default-branch commit is legitimate.
  if [ "$kind" = "R" ]; then
    if [ -z "${default_branch[$repo]:-}" ]; then
      api "repos/$repo"
      if [ "$HTTP_CODE" != "200" ]; then
        echo "  UNVERIFIED  $repo — default branch unknown (HTTP $HTTP_CODE)"
        unverified=$((unverified + 1))
        continue
      fi
      default_branch[$repo]="$(printf '%s' "$API_BODY" | jq -r '.default_branch // empty')"
    fi
    head="${default_branch[$repo]:-}"
    if [ -z "$head" ]; then
      echo "  UNVERIFIED  $repo — no default branch reported"
      unverified=$((unverified + 1))
      continue
    fi

    api "repos/$repo/compare/$head...$sha"
    if [ "$HTTP_CODE" != "200" ]; then
      echo "  UNVERIFIED  $spec@$short… — compare answered HTTP $HTTP_CODE"
      unverified=$((unverified + 1))
      continue
    fi
    status="$(printf '%s' "$API_BODY" | jq -r '.status // empty')"
    case "$status" in
      behind | identical)
        echo "  ok    $spec@$short… ($status $head, callable)"
        ;;
      ahead | diverged)
        echo "  FAIL  $spec@$short… — not an ancestor of $repo@$head ($status): a commit no default-branch ref reaches, so GitHub cannot resolve the called workflow"
        failed=$((failed + 1))
        ;;
      *)
        echo "  UNVERIFIED  $spec@$short… — compare returned status '$status'"
        unverified=$((unverified + 1))
        ;;
    esac
  else
    echo "  ok    $spec@$short…"
  fi
done <<<"$pins"

echo
if [ "$failed" -gt 0 ]; then
  echo "::error::$failed pin(s) are determinate negatives: the commit, or the action repository itself, does not exist. Actions resolves a uses: ref only at run time, and an unresolvable ref produces no check run at all — so this failure is invisible on the PR unless it is caught here."
  exit 1
fi
if [ "$unverified" -gt 0 ]; then
  echo "::warning::$unverified of $count pin(s) could not be verified — the API did not answer definitively (rate limit, 5xx, network). That is a gap in this gate, not a defect in the pin; it is reported rather than failed so a GitHub incident cannot redden every run."
  echo "verified $((count - unverified)) of $count pin(s); $unverified UNVERIFIED (see the warning above)"
  exit 0
fi
echo "all $count pin(s) resolve, and every reusable pin is callable from its default branch"
