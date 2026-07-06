#!/usr/bin/env bash
# test_factory_overlay.sh — round-trip tests for factory-overlay.sh
# Exercises the QUEUED → DISPATCHED → ATTESTED → READY flow and the helper
# subcommands (route-record, capacity, gate-assessment, reroll-verdict, park,
# bead-closed-check, tick-summary, recover-held, list).
#
# Run with: bash tests/scripts/test_factory_overlay.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"

# Isolated DB so we don't pollute the real CXDB.
export AFD_DB="/tmp/test-overlay-$$-$$.sqlite"
export AFD_LOG="/tmp/test-overlay-$$-$$.jsonl"
export CONFIG="$ROOT/daemon/contracts/daemon.toml.example"
# br needs a beads.db; point at a fresh temp one so bead-closed-check can run.
export BR_DB="/tmp/test-overlay-$$-beads.db"
touch "$BR_DB"

# Override br binary to a no-op shim that returns controllable JSON.
export BR_BIN="/tmp/test-overlay-$$-br.sh"
cat > "$BR_BIN" <<'BR_EOF'
#!/usr/bin/env bash
# Fake br shim: --json shows {status:"open"|"closed"} based on /tmp/br-status
case "${1:-}" in
  show)
    bead="$2"
    if [ "${3:-}" = "--json" ]; then
      status="$(cat /tmp/br-status 2>/dev/null || echo open)"
      printf '[{"id":"%s","status":"%s"}]\n' "$bead" "$status"
    fi
    ;;
esac
BR_EOF
chmod +x "$BR_BIN"

cleanup() { rm -f "$AFD_DB" "$AFD_LOG" "$BR_DB" "$BR_BIN" /tmp/br-status; }
trap cleanup EXIT

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

# 1. init
init_out="$("$OVERLAY" init 2>&1 | tail -1)"
assert "init returns ok" "ok: schema applied to $AFD_DB" "$init_out"

# 2. intake-upsert (new)
out="$("$OVERLAY" intake-upsert test-roundtrip 'round trip test bead')"
assert "intake-upsert creates new" "created" "$out"

# 3. intake-upsert idempotent
out="$("$OVERLAY" intake-upsert test-roundtrip 'again')"
assert "intake-upsert idempotent" "exists" "$out"

# 4. list QUEUED shows the new bead
listed="$(echo "$("$OVERLAY" list QUEUED)" | python3 -c 'import json,sys; print(",".join(b["bead_id"] for b in json.load(sys.stdin)))')"
assert "list QUEUED contains test-roundtrip" "test-roundtrip" "$listed"

# 5. route-record
out="$("$OVERLAY" route-record test-roundtrip STANDARD_PATH 'drive existing PR')"
assert "route-record STANDARD_PATH" "ok" "$out"

# 6. route-record rejects bad verdict
set +e
out="$("$OVERLAY" route-record test-roundtrip BAD_VERDICT 2>&1)"
rc=$?
set -e
assert "route-record rejects bad verdict" "1" "$rc"

# 7. capacity returns a number
cap="$("$OVERLAY" capacity)"
[[ "$cap" =~ ^[0-9]+$ ]] || { echo "FAIL: capacity not numeric: $cap"; FAIL=$((FAIL+1)); }
[ "$cap" -ge 1 ] && echo "PASS: capacity >= 1 ($cap)" && PASS=$((PASS+1))

# 8. dispatch-record QUEUED → DISPATCHED
out="$("$OVERLAY" dispatch-record test-roundtrip fix/test-branch)"
assert "dispatch-record ok" "ok" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-roundtrip';")"
assert "state after dispatch-record" "DISPATCHED" "$state"

# 9. dispatch-record same branch different bead should fail (already registered)
set +e
out="$("$OVERLAY" intake-upsert test-other 'another' 2>&1)"
out="$("$OVERLAY" route-record test-other STANDARD_PATH 2>&1)"
out="$("$OVERLAY" dispatch-record test-other fix/test-branch 2>&1)"
rc=$?
set -e
assert "dispatch-record rejects duplicate branch" "1" "$rc"

# 10. pr-opened DISPATCHED → ATTESTED
out="$("$OVERLAY" pr-opened test-roundtrip 7888 https://github.com/jleechanorg/worldarchitect.ai/pull/7888)"
assert "pr-opened ok" "ok" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-roundtrip';")"
assert "state after pr-opened" "ATTESTED" "$state"
pr="$(sqlite3 "$AFD_DB" "SELECT pr_number FROM bead_overlay WHERE bead_id='test-roundtrip';")"
assert "pr_number after pr-opened" "7888" "$pr"

# 11. gate-assessment ATTESTED, all-green
gates='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green"}'
out="$("$OVERLAY" gate-assessment test-roundtrip 7888 "$gates")"
assert "gate-assessment all-green → true" "true" "$(echo "$out" | head -1)"
assert "gate-assessment cooldown=false" "cooldown_ready=false" "$(echo "$out" | tail -1)"

# 12. ready ATTESTED → READY
out="$("$OVERLAY" ready test-roundtrip 7888)"
assert "ready ok" "ok" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-roundtrip';")"
assert "state after ready" "READY" "$state"

# 13. park (generic)
"$OVERLAY" intake-upsert test-park 'park test' >/dev/null
"$OVERLAY" route-record test-park SMALL_PATH >/dev/null
out="$("$OVERLAY" park test-park 'manual hold')"
assert "park ok" "ok" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-park';")"
assert "state after park" "HUMAN_HELD" "$state"

# 14. recover-held HUMAN_HELD → QUEUED
out="$("$OVERLAY" recover-held)"
[[ "$out" =~ recovered=1 ]] || { echo "FAIL: recover-held did not recover 1 (got $out)"; FAIL=$((FAIL+1)); }
[ $? -eq 0 ] && echo "PASS: recover-held recovered 1" && PASS=$((PASS+1))

# 15. tick-summary emits telemetry
echo "tick" > /tmp/br-status  # doesn't matter for tick-summary
out="$("$OVERLAY" tick-summary verifier)"
assert "tick-summary verifier" "ok" "$out"
ticked="$(grep -c '"eventType": "TICK"' "$AFD_LOG")"
[ "$ticked" -ge 1 ] && echo "PASS: TICK emitted ($ticked lines)" && PASS=$((PASS+1)) \
  || { echo "FAIL: no TICK in log"; FAIL=$((FAIL+1)); }

# 16. bead-closed-check on closed-after-merge → READY
"$OVERLAY" intake-upsert test-closed 'closed test' >/dev/null
"$OVERLAY" route-record test-closed STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-closed fix/closed-branch >/dev/null
"$OVERLAY" pr-opened test-closed 1234 https://github.com/jleechanorg/worldarchitect.ai/pull/1234 >/dev/null
# emit a clean gate-assessment first so closed-after-merge path triggers
gates='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green"}'
"$OVERLAY" gate-assessment test-closed 1234 "$gates" >/dev/null
echo "closed" > /tmp/br-status
out="$("$OVERLAY" bead-closed-check test-closed)"
assert "bead-closed-check → ready after merge" "ready" "$out"

# 17. park-duplicate
"$OVERLAY" intake-upsert test-dup 'dup test' >/dev/null
"$OVERLAY" route-record test-dup STANDARD_PATH >/dev/null
out="$("$OVERLAY" park-duplicate test-dup 'dup of parent')"
assert "park-duplicate" "parked test-dup" "$out"

# 18. redrive-pr
out="$("$OVERLAY" redrive-pr test-dup 9999 fix/redrive-branch)"
assert "redrive-pr ok" "redriven test-dup PR #9999" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-dup';")"
assert "state after redrive-pr" "QUEUED" "$state"

# 19. unstick-dispatching (no rows in DISPATCHING, should be 0)
out="$("$OVERLAY" unstick-dispatching)"
assert "unstick-dispatching 0" "unstuck=0" "$out"

# 20. invalid state rejection
set +e
out="$("$OVERLAY" list NOT_A_STATE 2>&1)"
rc=$?
set -e
assert "list rejects invalid state" "1" "$rc"

# 21. reroll-verdict (reroll_worthy → HUMAN_HELD)
"$OVERLAY" intake-upsert test-reroll 'reroll test' >/dev/null
"$OVERLAY" route-record test-reroll STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-reroll fix/reroll-branch >/dev/null
"$OVERLAY" pr-opened test-reroll 5555 https://github.com/jleechanorg/worldarchitect.ai/pull/5555 >/dev/null
out="$("$OVERLAY" reroll-verdict test-reroll 5555 reroll_worthy 'merge conflict blocker')"
assert "reroll-verdict ok" "ok" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-reroll';")"
assert "reroll_worthy → HUMAN_HELD" "HUMAN_HELD" "$state"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0