#!/usr/bin/env bash
# test_auto_merge_guard_exact_head.sh — regression coverage for the
# fail-closed exact-head 7-green merge authority hook in
# `daemon/scripts/auto-merge-guard.sh` (jleechan-goal-unattended-e2e-2026-07-17-bze8.1).
#
# These checks verify the SHELL-LEVEL contract:
#
#   1. The guard script still bash-parses cleanly.
#   2. The guard references the new merge-authority CLI module
#      (`python3 -m runner.merge_authority_cli`) so the bash hook
#      actually invokes the fail-closed authority before merging.
#   3. The guard refuses to fall back to a no-red predicate alone —
#      the exact-head authority must always be consulted.
#   4. A disposition-note-style bypass keyword (OPERATOR_OVERRIDE,
#      MERGE_ANYWAY) is NOT honored as a substitute for missing
#      evidence anywhere in the guard's predicate path.
#   5. The seven named gates from `daemon/src/verifier.rs` are
#      reflected in the bash hook's per-gate telemetry emission.
#   6. The PR293/PR300 regression class is named in the source so
#      the next maintainer can grep for it.
#
# Run with: bash tests/scripts/test_auto_merge_guard_exact_head.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"
AUTHORITY="$ROOT/runner/merge_authority.py"
CLI="$ROOT/runner/merge_authority_cli.py"

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"; FAIL=$((FAIL + 1))
  fi
}
assert_file_exists() {
  local name="$1" path="$2"
  if [ -f "$path" ]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (missing: $path)"; FAIL=$((FAIL + 1))
  fi
}
assert_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file"; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (missing pattern: $pattern in $file)"; FAIL=$((FAIL + 1))
  fi
}
assert_not_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file"; then
    echo "FAIL: $name (unexpected pattern: $pattern in $file)"; FAIL=$((FAIL + 1))
  else
    echo "PASS: $name"; PASS=$((PASS + 1))
  fi
}

assert_file_exists "auto-merge-guard.sh present" "$GUARD"
assert_file_exists "merge_authority.py present" "$AUTHORITY"
assert_file_exists "merge_authority_cli.py present" "$CLI"

if [ -f "$GUARD" ]; then
  if bash -n "$GUARD"; then
    echo "PASS: auto-merge-guard.sh bash -n clean"; PASS=$((PASS + 1))
  else
    echo "FAIL: auto-merge-guard.sh bash -n failed"; FAIL=$((FAIL + 1))
  fi
fi

# 1. The guard must invoke the merge-authority CLI module (the fail-closed
#    exact-head path) BEFORE merging. This is the headline structural
#    assertion — without this line the bash loop falls back to the legacy
#    no-red predicate alone, which is the regression class we are
#    closing.
assert_grep "guard invokes merge_authority_cli module" \
  "python3 -m runner\.merge_authority_cli" "$GUARD"

# 2. The guard must capture the live head SHA via `gh pr view` before
#    invoking the authority so the verdict is bound to the live PR head,
#    not whatever SHA the caller last cached.
assert_grep "guard resolves live head SHA via gh pr view" \
  "gh pr view.*headRefOid" "$GUARD"

# 3. The guard must NOT bypass the authority on a disposition note or
#    operator override. The shell-level predicate treats a disposition
#    note as a comment field, not a gate substitute.
assert_not_grep "guard does not bypass on OPERATOR_OVERRIDE" \
  "OPERATOR_OVERRIDE.*ALLOW|OPERATOR_OVERRIDE.*BYPASS" "$GUARD"
assert_not_grep "guard does not bypass on MERGE_ANYWAY" \
  "MERGE_ANYWAY" "$GUARD"

# 4. The guard's per-gate telemetry emission names the seven required
#    gates by their canonical verifier.rs vocabulary.
for g in ci_green no_conflicts coderabbit bugbot comments_resolved evidence_review skeptic; do
  assert_grep "guard emits per-gate telemetry for $g" \
    "$g" "$GUARD"
done

# 5. The guard references the PR293/PR300 regression class so the next
#    maintainer can grep for it.
assert_grep "guard documents PR293 regression class" \
  "PR293|PR300|jleechan-goal-unattended-e2e-2026-07-17-bze8\.1" "$GUARD"

# 6. The merge_authority.py source declares exactly seven gates.
if [ -f "$AUTHORITY" ]; then
  seven_count="$(grep -c '^    [A-Z_]* = "[a-z_]*",$' "$AUTHORITY" | head -1 || true)"
  assert_grep "authority declares CI gate" '^    CI = "ci_green"' "$AUTHORITY"
  assert_grep "authority declares SKEPTIC gate" '^    SKEPTIC = "skeptic"' "$AUTHORITY"
  assert_grep "authority uses closed GateStatus enum" 'class GateStatus' "$AUTHORITY"
  assert_grep "authority uses closed MergeVerdict enum" 'class MergeVerdict' "$AUTHORITY"
  assert_grep "authority SHA-binding rule documented" \
    'stale-SHA|stale SHA|stale.head|head_sha' "$AUTHORITY"
  assert_grep "authority disposition rule documented" \
    'disposition' "$AUTHORITY"
fi

# 7. The CLI module emits per-gate telemetry.
if [ -f "$CLI" ]; then
  assert_grep "CLI emits source_actor in telemetry" "source_actor" "$CLI"
  assert_grep "CLI emits source_url in telemetry"  "source_url"  "$CLI"
  assert_grep "CLI emits source_id in telemetry"   "source_id"   "$CLI"
  assert_grep "CLI emits head_sha in telemetry"    "head_sha"    "$CLI"
  assert_grep "CLI emits observed_at in telemetry" "observed_at" "$CLI"
  assert_grep "CLI refuses to honor a CI status check as CodeRabbit approval" \
    "review:APPROVED" "$CLI"
  assert_grep "CLI enforces Bugbot error-count zero" \
    "bugbot_error_count" "$CLI"
fi

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
