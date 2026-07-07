#!/usr/bin/env bash
# overlay-harness-check.sh — vendored portion of
# ~/.claude/skills/callpath/profiles/dark-factory/run.sh that probes
# daemon/factory-overlay.sh for the required subcommands.
#
# Exists in the repo (not user-scope) so tests/scripts/test_callpath_overlay_harness.sh
# and any CI gate can run the probe in a clean environment without depending on
# user-scope files. See bead jleechan-8xxl.
#
# Usage: overlay-harness-check.sh <path-to-factory-overlay.sh>
#   prints "ok/<N>" on success, "missing:<sub>" on first missing subcommand,
#   "missing:overlay" if the script doesn't exist or isn't executable.
#   exits 0 on ok, 1 on missing.
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 <path-to-factory-overlay.sh>" >&2
    exit 64
fi

OVERLAY_PATH="$1"

# Required subcommands per bead jleechan-df94 and PR #167 (commit 10dc5b16a).
# KEEP IN SYNC with daemon/factory-overlay.sh source of truth. If a subcommand
# is added/removed in factory-overlay.sh, update this list AND the callpath
# user-scope profile in the same commit. The CI gate
# tests/scripts/test_callpath_overlay_harness.sh verifies the count.
OVERLAY_HARNESS_SUBCOMMANDS=(
  init
  intake-upsert
  route-record
  capacity
  dispatch-record
  pr-opened
  autonomy-tick
  gate-assessment
  prev-gate-assessment
  ready
  reroll-verdict
  park
  park-duplicate
  bead-closed-check
  tick-summary
  recover-held
  unstick-dispatching
  redrive-pr
  list
)

if [[ ! -x "$OVERLAY_PATH" ]]; then
    echo "missing:overlay"
    exit 1
fi

for sub in "${OVERLAY_HARNESS_SUBCOMMANDS[@]}"; do
    if ! grep -qE "^${sub}\)" "$OVERLAY_PATH" 2>/dev/null; then
        echo "missing:${sub}"
        exit 1
    fi
done

echo "ok/${#OVERLAY_HARNESS_SUBCOMMANDS[@]}"
exit 0