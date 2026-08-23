#!/usr/bin/env bash
# check_runner_health.sh — pre-merge smoke step to check online runner count
# and surface runner outages before merge deadlock.
set -euo pipefail

REPO="${1:-$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo jleechanorg/dark-factory)}"
ONLINE_COUNT="$(gh api "repos/$REPO/actions/runners" --jq '[.runners[]? | select(.status=="online")] | length' 2>/dev/null || echo "0")"

echo "Online runners in $REPO pool: $ONLINE_COUNT"
if [ "$ONLINE_COUNT" -eq 0 ]; then
  echo "RUNNER OUTAGE — consider --admin or wait"
  exit 1
fi
exit 0
