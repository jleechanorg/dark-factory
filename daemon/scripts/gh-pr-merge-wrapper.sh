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

echo "gh-pr-merge-wrapper: merging PR $PR (gh pr merge $*)"
if [ -n "$REPO" ]; then
  exec gh pr merge "$PR" --repo "$REPO" "$@"
else
  exec gh pr merge "$PR" "$@"
fi
