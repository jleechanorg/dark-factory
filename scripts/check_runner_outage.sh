#!/usr/bin/env bash
# Pre-merge smoke step: check online runner count and surface outages (Candidate A).
#
# Usage:
#   scripts/check_runner_outage.sh [owner/repo] [pr_number]

set -euo pipefail

REPO="${1:-jleechanorg/dark-factory}"
PR_NUMBER="${2:-}"

# Check online runner count via GitHub API
RUNNERS_ONLINE="$(gh api "/repos/${REPO}/actions/runners" --jq '[.runners[]? | select(.status=="online")] | length' 2>/dev/null || echo "0")"

# If 0, fallback to check org-level runners
if [ "$RUNNERS_ONLINE" -eq 0 ]; then
  ORG="${REPO%%/*}"
  if [ -n "$ORG" ]; then
    RUNNERS_ONLINE="$(gh api "/orgs/${ORG}/actions/runners?per_page=100" --jq '[.runners[]? | select(.status=="online")] | length' 2>/dev/null || echo "0")"
  fi
fi

if [ "$RUNNERS_ONLINE" -eq 0 ]; then
  echo "⚠️ RUNNER OUTAGE — consider --admin or wait" >&2
  if [ -n "$PR_NUMBER" ]; then
    gh pr comment "$PR_NUMBER" --repo "$REPO" --body "⚠️ **[dark-factory /pre-merge]** RUNNER OUTAGE — consider --admin or wait" || true
  fi
  exit 3
fi

echo "OK: ${RUNNERS_ONLINE} online runners available for ${REPO}."
exit 0
