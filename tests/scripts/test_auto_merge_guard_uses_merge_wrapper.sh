#!/usr/bin/env bash
# test_auto_merge_guard_uses_merge_wrapper.sh — TDD coverage for bead
# rev-pkojz's integration gap: the new daemon/scripts/gh-pr-merge-wrapper.sh
# (auto-promotes draft PRs before merge) existed but nothing called it —
# daemon/scripts/auto-merge-guard.sh, the ONLY automated merge path in this
# repo, still called `gh pr merge` directly with no draft check, so the
# original PR #9100 incident (admin merge failing on a draft PR) remained
# unprotected by the automated flow. This proves the wiring, statically:
# the merge-path line in auto-merge-guard.sh must invoke the wrapper script,
# not a raw `gh pr merge` call.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"
WRAPPER="$ROOT/daemon/scripts/gh-pr-merge-wrapper.sh"

PASS=0; FAIL=0
assert_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected to find '$needle')"; FAIL=$((FAIL + 1))
  fi
}
assert_not_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "FAIL: $name (unexpected: found '$needle')"; FAIL=$((FAIL + 1))
  else
    echo "PASS: $name"; PASS=$((PASS + 1))
  fi
}

[ -f "$WRAPPER" ] || { echo "FAIL: wrapper script missing at $WRAPPER"; exit 1; }
[ -x "$WRAPPER" ] || { echo "FAIL: wrapper script not executable"; exit 1; }
echo "PASS: wrapper script exists and is executable"; PASS=$((PASS + 1))

CONTENT="$(cat "$GUARD")"
assert_contains "guard invokes gh-pr-merge-wrapper.sh on the merge path" \
  "gh-pr-merge-wrapper.sh" "$CONTENT"
assert_not_contains "guard no longer calls raw 'gh pr merge' directly" \
  'gh pr merge "$num"' "$CONTENT"

echo ""
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
