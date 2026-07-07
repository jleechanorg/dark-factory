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

# 11. gate-assessment ATTESTED, all-green (9-gate schema: 7 originals + code_standards + zfc)
gates='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","code_standards":"green","zfc":"green"}'
out="$("$OVERLAY" gate-assessment test-roundtrip 7888 "$gates")"
assert "gate-assessment all-green → true" "true" "$(echo "$out" | head -1)"
assert "gate-assessment cooldown=false" "cooldown_ready=false" "$(echo "$out" | tail -1)"

# 11a. gate-assessment ATTESTED, 9-gate schema (with /code-standards + /zfc)
# Pull the bead back to ATTESTED to re-run gate-assessment (was READY).
"$OVERLAY" intake-upsert test-9gates 'nine gate test' >/dev/null
"$OVERLAY" route-record test-9gates STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-9gates fix/nine-gates >/dev/null
"$OVERLAY" pr-opened test-9gates 7889 https://github.com/jleechanorg/worldarchitect.ai/pull/7889 >/dev/null
gates9='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","code_standards":"green","zfc":"green"}'
out9="$("$OVERLAY" gate-assessment test-9gates 7889 "$gates9")"
assert "gate-assessment 9-gate schema → true" "true" "$(echo "$out9" | head -1)"

# 11b. gate-assessment red on /zfc → false (verifies per-gate precedence)
"$OVERLAY" intake-upsert test-zfc-red 'zfc red test' >/dev/null
"$OVERLAY" route-record test-zfc-red STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-zfc-red fix/zfc-red >/dev/null
"$OVERLAY" pr-opened test-zfc-red 7890 https://github.com/jleechanorg/worldarchitect.ai/pull/7890 >/dev/null
gates_zfc_red='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","code_standards":"green","zfc":"red"}'
out_zfc="$("$OVERLAY" gate-assessment test-zfc-red 7890 "$gates_zfc_red")"
assert "gate-assessment 9-gate w/ zfc=red → false" "false" "$(echo "$out_zfc" | head -1)"

# 11c. gate-assessment rejects missing new keys (legacy 7-gate JSON should fail)
"$OVERLAY" intake-upsert test-legacy 'legacy 7-gate test' >/dev/null
"$OVERLAY" route-record test-legacy STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-legacy fix/legacy-gates >/dev/null
"$OVERLAY" pr-opened test-legacy 7891 https://github.com/jleechanorg/worldarchitect.ai/pull/7891 >/dev/null
gates_legacy7='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green"}'
set +e
"$OVERLAY" gate-assessment test-legacy 7891 "$gates_legacy7" 2>&1
rc_legacy=$?
set -e
assert "gate-assessment rejects legacy 7-gate schema" "1" "$rc_legacy"

# 11d. gate-assessment accepts pass/warn/fail (jleechan-240 expansion).
# Warn is NON-blocking: a single warn still yields all_green=true so the
# verifier can route it through the bounded fix loop without parking
# the bead.
"$OVERLAY" intake-upsert test-pass-warn 'pass/warn enum test' >/dev/null
"$OVERLAY" route-record test-pass-warn STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-pass-warn fix/pass-warn-branch >/dev/null
"$OVERLAY" pr-opened test-pass-warn 7892 https://github.com/jleechanorg/worldarchitect.ai/pull/7892 >/dev/null
gates_pw='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"warn"}'
out_pw="$("$OVERLAY" gate-assessment test-pass-warn 7892 "$gates_pw")"
assert "gate-assessment warn verdict → all_green true" "true" "$(echo "$out_pw" | head -1)"

# 11e. structured evidence: each gate value may also be an object
# {"verdict":..., "evidence":[...]}. The verdict drives all_green; the
# evidence array passes through to CXDB so reviewers can audit the gate
# without re-running the model.
"$OVERLAY" intake-upsert test-evidence 'structured evidence test' >/dev/null
"$OVERLAY" route-record test-evidence STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-evidence fix/evidence-branch >/dev/null
"$OVERLAY" pr-opened test-evidence 7893 https://github.com/jleechanorg/worldarchitect.ai/pull/7893 >/dev/null
gates_ev='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":{"verdict":"pass","evidence":[{"path":"runner/handlers.py","line":42,"msg":"no banned patterns"}]}}'
out_ev="$("$OVERLAY" gate-assessment test-evidence 7893 "$gates_ev")"
assert "gate-assessment structured-evidence → all_green true" "true" "$(echo "$out_ev" | head -1)"

# 11f. structured evidence with fail verdict → all_green=false
"$OVERLAY" intake-upsert test-ev-fail 'structured evidence fail test' >/dev/null
"$OVERLAY" route-record test-ev-fail STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-ev-fail fix/ev-fail-branch >/dev/null
"$OVERLAY" pr-opened test-ev-fail 7894 https://github.com/jleechanorg/worldarchitect.ai/pull/7894 >/dev/null
gates_evf='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":{"verdict":"fail","evidence":[{"path":"daemon/factory-overlay.sh","line":201,"msg":"keyword blacklist enforced in app code"}]}}'
out_evf="$("$OVERLAY" gate-assessment test-ev-fail 7894 "$gates_evf")"
assert "gate-assessment fail+evidence → all_green false" "false" "$(echo "$out_evf" | head -1)"

# 11g. bounded fix loop (jleechan-240): a fail gate → reroll-verdict
# (reroll_worthy) → HUMAN_HELD → recover-held → QUEUED. This proves the
# new /code-standards + /zfc gates route failed verdicts through the same
# bounded fix loop as the original 7 gates (no parallel implementation).
"$OVERLAY" reroll-verdict test-ev-fail 7894 reroll_worthy "zfc: keyword in app code" >/dev/null
state_evf="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-ev-fail';")"
assert "zfc fail → reroll-verdict → HUMAN_HELD" "HUMAN_HELD" "$state_evf"
out_rec="$(echo open > /tmp/br-status; "$OVERLAY" recover-held)"
[[ "$out_rec" =~ recovered=1 ]] || { echo "FAIL: recover-held did not recover (got $out_rec)"; FAIL=$((FAIL+1)); }
[ $? -eq 0 ] && echo "PASS: recover-held returned bead to QUEUED via bounded fix loop" && PASS=$((PASS+1))
state_evf2="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-ev-fail';")"
assert "zfc fix-loop → recover-held → QUEUED" "QUEUED" "$state_evf2"

# 11h. unknown verdict waits for the next tick (does NOT falsely pretend
# readiness). all_green=false when any gate is unknown; the verifier
# re-dispatches reviews and re-runs gate-assessment in the next tick.
"$OVERLAY" intake-upsert test-unknown 'unknown verdict test' >/dev/null
"$OVERLAY" route-record test-unknown STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-unknown fix/unknown-branch >/dev/null
"$OVERLAY" pr-opened test-unknown 7895 https://github.com/jleechanorg/worldarchitect.ai/pull/7895 >/dev/null
gates_uk='{"ci_green":"unknown","no_conflicts":"unknown","coderabbit":"unknown","bugbot":"unknown","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","code_standards":"unknown","zfc":"unknown"}'
out_uk="$("$OVERLAY" gate-assessment test-unknown 7895 "$gates_uk")"
assert "gate-assessment all-unknown → all_green false (waits for tick)" "false" "$(echo "$out_uk" | head -1)"

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
gates='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","code_standards":"green","zfc":"green"}'
"$OVERLAY" gate-assessment test-closed 1234 "$gates" >/dev/null
echo "closed" > /tmp/br-status
out="$("$OVERLAY" bead-closed-check test-closed)"
assert "bead-closed-check → ready after merge" "ready" "$out"

# 16a. bead-closed-check on closed-after-merge WITH a fail gate → parked
# (not ready). jleechan-240 follow-up: the bead-closed-check consumer
# previously only blocked the literal "red" string. With the new fail
# verdict vocabulary, a fail-gate assessment must park the bead instead
# of marking it READY.
"$OVERLAY" intake-upsert test-closed-fail 'closed-fail test' >/dev/null
"$OVERLAY" route-record test-closed-fail STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-closed-fail fix/closed-fail-branch >/dev/null
"$OVERLAY" pr-opened test-closed-fail 1235 https://github.com/jleechanorg/worldarchitect.ai/pull/1235 >/dev/null
gates_fail='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"fail"}'
"$OVERLAY" gate-assessment test-closed-fail 1235 "$gates_fail" >/dev/null
echo "closed" > /tmp/br-status
out_fail="$("$OVERLAY" bead-closed-check test-closed-fail)"
assert "bead-closed-check w/ zfc=fail → parked (jleechan-240)" "parked" "$out_fail"
state_fail="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-closed-fail';")"
assert "fail-gate closed-bead → HUMAN_HELD (not READY)" "HUMAN_HELD" "$state_fail"

# 16b. bead-closed-check on closed-after-merge WITH a structured fail
# evidence object → parked. Verifies the {verdict: "fail", evidence:[...]}
# shape is honored by the consumer (not just the plain "fail" string).
"$OVERLAY" intake-upsert test-closed-evfail 'closed-evfail test' >/dev/null
"$OVERLAY" route-record test-closed-evfail STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-closed-evfail fix/closed-evfail-branch >/dev/null
"$OVERLAY" pr-opened test-closed-evfail 1236 https://github.com/jleechanorg/worldarchitect.ai/pull/1236 >/dev/null
gates_evfail='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":{"verdict":"fail","evidence":[{"path":"daemon/factory-overlay.sh","line":201,"msg":"keyword in app code"}]}}'
"$OVERLAY" gate-assessment test-closed-evfail 1236 "$gates_evfail" >/dev/null
echo "closed" > /tmp/br-status
out_evfail="$("$OVERLAY" bead-closed-check test-closed-evfail)"
assert "bead-closed-check w/ zfc={verdict:fail} → parked" "parked" "$out_evfail"

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