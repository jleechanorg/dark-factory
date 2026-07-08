#!/usr/bin/env bash
# test_rollback_dispatched.sh — round-trip tests for factory-overlay.sh
# rollback-dispatched subcommand (Codex P1 finding on PR #193).
#
# Why this exists
# ---------------
# factory-ao-remediate.sh (async mode) writes a state file when the detached
# `ao spawn` finishes. If the spawn fails AFTER the fast-fail window (slow
# internal error, daemon dies mid-spawn), the bead is stranded as DISPATCHED
# with no AO session. The next tick's `rollback-dispatched` subcommand must:
#   1. Read each DISPATCHED bead's spawn-state file.
#   2. Roll the bead back to QUEUED when the state file shows "fail:rc=N".
#   3. Leave beads alone when the state file shows "ok" or "pending".
#   4. Emit a ROLLBACK_DISPATCHED telemetry event.
#
# Tests
# -----
# 1. happy: DISPATCHED + fail state → QUEUED + telemetry
# 2. happy: DISPATCHED + ok state → unchanged
# 3. happy: DISPATCHED + pending state → unchanged
# 4. happy: DISPATCHED + no state file → unchanged (no spurious rollbacks)
# 5. happy: multiple DISPATCHED beads, mixed state files → only fail:* rolls back
#
# Run with: bash tests/scripts/test_rollback_dispatched.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"
    FAIL=$((FAIL + 1))
  fi
}
assert_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file" 2>/dev/null; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (pattern '$pattern' not found in $file)"
    FAIL=$((FAIL + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-rollback.XXXXXX)"
SPAWN_STATE_DIR="$SCRATCH_DIR/spawns"
mkdir -p "$SPAWN_STATE_DIR"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

# Helper: fresh DB + log + apply schema; sets AFD_DB / AFD_LOG accordingly.
fresh_db() {
  local tag="${1:-main}"
  export AFD_DB="$SCRATCH_DIR/cxdb-$tag.sqlite"
  export AFD_LOG="$SCRATCH_DIR/cxdb-$tag.jsonl"
  "$OVERLAY" init >/dev/null
}

# ---------------------------------------------------------------------------
# Test 1: DISPATCHED bead with fail state file → rolls back to QUEUED
# ---------------------------------------------------------------------------
fresh_db happy
"$OVERLAY" intake-upsert test-happy 'rollback happy' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9001, branch='fix/test-happy-branch', state='DISPATCHED' WHERE bead_id='test-happy';"
# Branch registry row required for dispatch-record flow but not for rollback.
sqlite3 "$AFD_DB" "INSERT INTO branch_registry (branch, bead_id, registered_at) VALUES ('fix/test-happy-branch', 'test-happy', '$(date -u +%Y-%m-%dT%H:%M:%SZ)');" 2>/dev/null || true
# Pre-seed the spawn state file with a failure.
echo "fail:rc=3" > "$SPAWN_STATE_DIR/test-happy-9001.state"

export AFD_SPAWN_STATE_DIR="$SPAWN_STATE_DIR"
out="$("$OVERLAY" rollback-dispatched 2>&1)"
case "$out" in
  *rolled=1*) echo "PASS: overlay reports rolled=1"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: overlay reports wrong count: $out"; FAIL=$((FAIL + 1)) ;;
esac
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-happy';")"
assert "state DISPATCHED → QUEUED (rollback)" "QUEUED" "$state"
assert_grep "ROLLBACK_DISPATCHED telemetry emitted" '"eventType": "ROLLBACK_DISPATCHED"' "$AFD_LOG"

# ---------------------------------------------------------------------------
# Test 2: DISPATCHED bead with ok state file → unchanged
# ---------------------------------------------------------------------------
fresh_db ok
"$OVERLAY" intake-upsert test-ok 'rollback ok' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9002, branch='fix/test-ok-branch', state='DISPATCHED' WHERE bead_id='test-ok';"
echo "ok" > "$SPAWN_STATE_DIR/test-ok-9002.state"
out="$("$OVERLAY" rollback-dispatched 2>&1)"
case "$out" in
  *rolled=0*) echo "PASS: overlay reports rolled=0 (ok state untouched)"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: overlay reports wrong count: $out"; FAIL=$((FAIL + 1)) ;;
esac
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-ok';")"
assert "state unchanged when ok" "DISPATCHED" "$state"

# ---------------------------------------------------------------------------
# Test 3: DISPATCHED bead with pending state file → unchanged
# ---------------------------------------------------------------------------
fresh_db pending
"$OVERLAY" intake-upsert test-pending 'rollback pending' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9003, branch='fix/test-pending-branch', state='DISPATCHED' WHERE bead_id='test-pending';"
echo "pending" > "$SPAWN_STATE_DIR/test-pending-9003.state"
out="$("$OVERLAY" rollback-dispatched 2>&1)"
case "$out" in
  *rolled=0*) echo "PASS: overlay reports rolled=0 (pending state untouched)"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: overlay reports wrong count: $out"; FAIL=$((FAIL + 1)) ;;
esac
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-pending';")"
assert "state unchanged when pending" "DISPATCHED" "$state"

# ---------------------------------------------------------------------------
# Test 4: DISPATCHED bead with NO state file → unchanged (no spurious rollback)
# ---------------------------------------------------------------------------
fresh_db missing
"$OVERLAY" intake-upsert test-missing 'rollback missing' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9004, branch='fix/test-missing-branch', state='DISPATCHED' WHERE bead_id='test-missing';"
# NO state file for this bead
out="$("$OVERLAY" rollback-dispatched 2>&1)"
case "$out" in
  *rolled=0*) echo "PASS: overlay reports rolled=0 (no state file = no rollback)"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: overlay reports wrong count: $out"; FAIL=$((FAIL + 1)) ;;
esac
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-missing';")"
assert "state unchanged when no state file" "DISPATCHED" "$state"

# ---------------------------------------------------------------------------
# Test 5: mixed DISPATCHED beads → only fail:* rolls back
# ---------------------------------------------------------------------------
fresh_db mixed
"$OVERLAY" intake-upsert test-mixed-1 'mixed 1' >/dev/null
"$OVERLAY" intake-upsert test-mixed-2 'mixed 2' >/dev/null
"$OVERLAY" intake-upsert test-mixed-3 'mixed 3' >/dev/null
"$OVERLAY" intake-upsert test-mixed-4 'mixed 4' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9101, branch='fix/mixed-1', state='DISPATCHED' WHERE bead_id='test-mixed-1';"
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9102, branch='fix/mixed-2', state='DISPATCHED' WHERE bead_id='test-mixed-2';"
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9103, branch='fix/mixed-3', state='DISPATCHED' WHERE bead_id='test-mixed-3';"
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9104, branch='fix/mixed-4', state='DISPATCHED' WHERE bead_id='test-mixed-4';"
echo "fail:rc=7" > "$SPAWN_STATE_DIR/test-mixed-1-9101.state"  # ROLLBACK
echo "ok"         > "$SPAWN_STATE_DIR/test-mixed-2-9102.state"  # leave alone
echo "pending"    > "$SPAWN_STATE_DIR/test-mixed-3-9103.state"  # leave alone
# test-mixed-4 has NO state file                                       # leave alone
out="$("$OVERLAY" rollback-dispatched 2>&1)"
case "$out" in
  *rolled=1*) echo "PASS: mixed: only 1 bead rolled back"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: mixed: wrong rolled count: $out"; FAIL=$((FAIL + 1)) ;;
esac
state1="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-mixed-1';")"
state2="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-mixed-2';")"
state3="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-mixed-3';")"
state4="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-mixed-4';")"
assert "mixed: test-mixed-1 (fail) → QUEUED"  "QUEUED"     "$state1"
assert "mixed: test-mixed-2 (ok) → DISPATCHED" "DISPATCHED" "$state2"
assert "mixed: test-mixed-3 (pending) → DISPATCHED" "DISPATCHED" "$state3"
assert "mixed: test-mixed-4 (no file) → DISPATCHED" "DISPATCHED" "$state4"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0