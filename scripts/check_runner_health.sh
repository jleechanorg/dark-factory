#!/usr/bin/env bash
# check_runner_health.sh — verify the configured selector against org runners.
set -euo pipefail

REPO="${1:-$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo jleechanorg/dark-factory)}"
ORG="${REPO%%/*}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

set +e
RESULT="$(GITHUB_REPOSITORY="$REPO" python3 "$ROOT/scripts/check_runner_selector.py" --org "$ORG" --json 2>&1)"
RC=$?
set -e
printf '%s\n' "$RESULT"

case "$RC" in
  0) echo "Runner selector health: PASS for $REPO (org-scoped pool)" ;;
  1) echo "RUNNER SELECTOR DRIFT — configured labels match no online org runner" ;;
  2) echo "RUNNER STATUS INCONCLUSIVE — selector or GitHub API probe failed" ;;
  3) echo "RUNNER FLEET DOWN — wait for org runner recovery; merge policy remains enforced" ;;
  *) echo "RUNNER STATUS INCONCLUSIVE — unexpected verifier exit $RC" ;;
esac
exit "$RC"
