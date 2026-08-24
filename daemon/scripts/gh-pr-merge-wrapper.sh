#!/usr/bin/env bash
# gh-pr-merge-wrapper.sh — gh pr merge wrapper: auto-promote draft PRs
# before merge (bead rev-pkojz).
#
# ROOT-CAUSE: `gh pr merge <PR> --admin --squash` fails silently with a
# GraphQL "Pull Request is still a draft (mergePullRequest)" error when the
# target PR is still in draft state. The operator had to notice the failure
# and run `gh pr ready <PR>` manually before retrying — a 2-step process
# that breaks the "just merge it" mental model. Observed at 02:15Z
# 2026-08-23: PR #9100 admin merge failed for exactly this reason.
#
# FIX: this is a drop-in wrapper for `gh pr merge`. It takes the same <PR>
# argument plus any `gh pr merge` flags, checks whether the PR is a draft,
# promotes it via `gh pr ready <PR>` first when needed (logging that it did
# so), then runs the real `gh pr merge` with the original flags unchanged.
# Non-draft PRs merge exactly as before — no extra `gh pr ready` call, no
# extra API round trip beyond the one draft-state read.
#
# Usage: daemon/scripts/gh-pr-merge-wrapper.sh <PR> [gh pr merge flags...]
#   e.g. daemon/scripts/gh-pr-merge-wrapper.sh 9100 --admin --squash
#
# PROVENANCE (bead rev-377j4, 2026-08-23 PR-merge-storm postmortem): this
# script is the ONE place that actually invokes `gh pr merge` (auto-merge-
# guard.sh always calls through here — see test_auto_merge_guard_uses_merge_
# wrapper.sh). Establishing which host/script performed a batch of
# unattended merges took 1+ hour of cross-host detective work because
# nothing but human-readable `echo` lines were ever logged. Every merge
# invocation now appends ONE structured JSONL line to $AFD_LOG BEFORE the
# `gh pr merge` call: {ts, hostname, script_path,
# repo_git_sha_at_invocation, pr_number, triggering_mechanism}.
# `triggering_mechanism` is read from $AMG_TRIGGERING_MECHANISM (set by
# auto-merge-guard.sh to "auto-merge-guard.sh" when it invokes this
# wrapper) and defaults to "manual-wrapper-invocation" for any other
# caller.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: $0 <PR> [gh pr merge flags...]" >&2
  exit 2
fi

PR="$1"
shift

REPO="${GH_REPO:-}"
if [ -z "$REPO" ]; then
  REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)"
fi

_view_args=(pr view "$PR" --json isDraft --jq .isDraft)
[ -n "$REPO" ] && _view_args=(pr view "$PR" --repo "$REPO" --json isDraft --jq .isDraft)

is_draft="$(gh "${_view_args[@]}" 2>/dev/null || true)"

if [ "$is_draft" = "true" ]; then
  echo "gh-pr-merge-wrapper: PR $PR is a draft — promoting via 'gh pr ready $PR' before merge"
  if [ -n "$REPO" ]; then
    gh pr ready "$PR" --repo "$REPO"
  else
    gh pr ready "$PR"
  fi
  echo "gh-pr-merge-wrapper: PR $PR promoted from draft"
else
  echo "gh-pr-merge-wrapper: PR $PR is not a draft — no promotion needed"
fi

# --- Merge-authority provenance logging (bead rev-377j4) ---
_amg_log_merge_provenance() {
  local log_path script_path repo_root repo_sha ts hostname_val mechanism
  log_path="${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
  script_path="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")" || script_path="${BASH_SOURCE[0]}"
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && git rev-parse --show-toplevel 2>/dev/null)" || repo_root=""
  repo_sha="unknown"
  if [ -n "$repo_root" ]; then
    repo_sha="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null)" || repo_sha="unknown"
  fi
  ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)" || ts="unknown"
  hostname_val="${HOSTNAME:-}"
  if [ -z "$hostname_val" ]; then
    hostname_val="$(hostname 2>/dev/null)" || hostname_val=""
  fi
  [ -n "$hostname_val" ] || hostname_val="unknown"
  mechanism="${AMG_TRIGGERING_MECHANISM:-manual-wrapper-invocation}"
  mkdir -p "$(dirname "$log_path")" 2>/dev/null || true
  PR_NUM="$PR" TS="$ts" HOST="$hostname_val" SCRIPT="$script_path" SHA="$repo_sha" MECH="$mechanism" \
    python3 -c '
import json, os
pr = os.environ["PR_NUM"]
try:
    pr = int(pr)
except ValueError:
    pass
print(json.dumps({
    "ts": os.environ["TS"],
    "hostname": os.environ["HOST"],
    "script_path": os.environ["SCRIPT"],
    "repo_git_sha_at_invocation": os.environ["SHA"],
    "pr_number": pr,
    "triggering_mechanism": os.environ["MECH"],
}))
' >> "$log_path"
}
# Provenance logging is a best-effort side channel (same philosophy as
# daemon/src/telemetry.rs emission call sites): a logging failure must
# never block the actual merge.
_amg_log_merge_provenance || echo "gh-pr-merge-wrapper: WARNING failed to write merge-provenance log to ${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}" >&2

echo "gh-pr-merge-wrapper: merging PR $PR (gh pr merge $*)"
if [ -n "$REPO" ]; then
  exec gh pr merge "$PR" --repo "$REPO" "$@"
else
  exec gh pr merge "$PR" "$@"
fi
