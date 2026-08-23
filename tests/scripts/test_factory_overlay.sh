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
export AFD_DAEMON_BIN="${AFD_DAEMON_BIN:-$ROOT/daemon/target/debug/daemon}"
if [ ! -x "$AFD_DAEMON_BIN" ]; then
  # Bead jleechan-kn5j: the `test` job is the python lane and has no Rust
  # toolchain (only `daemon-tests` installs one), so a bare `cargo build` here
  # died with "line 18: cargo: command not found" and took the whole suite with
  # it. Skip loudly instead of failing: this suite needs a daemon binary it
  # cannot build, and `daemon-tests` covers that lane.
  if ! command -v cargo >/dev/null 2>&1; then
    echo "SKIP: cargo not on PATH and no prebuilt daemon at $AFD_DAEMON_BIN"
    exit 0
  fi
  cargo build --quiet --manifest-path "$ROOT/daemon/Cargo.toml"
fi
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

# CR-6: dispatch-record on a non-existent bead must return rc=8 (EX_NOT_FOUND),
# not rc=5 (EX_REQUIRE_STATE). The current implementation conflates "no row"
# with "wrong state" — both produce empty `cur` from get_field.
set +e
out="$("$OVERLAY" dispatch-record jleechan-doesnot-exist fix/never >/dev/null 2>&1)"
rc=$?
set -e
assert "dispatch-record missing bead returns rc=8 EX_NOT_FOUND" "8" "$rc"

# 7. capacity returns a number
cap="$("$OVERLAY" capacity)"
[[ "$cap" =~ ^[0-9]+$ ]] || { echo "FAIL: capacity not numeric: $cap"; FAIL=$((FAIL+1)); }
[ "$cap" -ge 1 ] && echo "PASS: capacity >= 1 ($cap)" && PASS=$((PASS+1))

# 8. dispatch-record QUEUED → DISPATCHED
out="$("$OVERLAY" dispatch-record test-roundtrip fix/test-branch)"
assert "dispatch-record ok" "ok" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-roundtrip';")"
assert "state after dispatch-record" "DISPATCHED" "$state"
autonomy_secs="$(sqlite3 "$AFD_DB" "SELECT autonomy_secs FROM bead_overlay WHERE bead_id='test-roundtrip';")"
assert "autonomy_secs reset after dispatch-record" "0" "$autonomy_secs"
started_at="$(sqlite3 "$AFD_DB" "SELECT attempt_started_at FROM bead_overlay WHERE bead_id='test-roundtrip';")"
[[ "$started_at" =~ ^[0-9]+$ ]] && assert "attempt_started_at stamped after dispatch-record" "1" "1"

# 9. dispatch-record same branch different bead should fail (already registered)
set +e
out="$("$OVERLAY" intake-upsert test-other 'another' 2>&1)"
out="$("$OVERLAY" route-record test-other STANDARD_PATH 2>&1)"
out="$("$OVERLAY" dispatch-record test-other fix/test-branch 2>&1)"
rc=$?
set -e
assert "dispatch-record rejects duplicate branch (rc=4 EX_BRANCH_CONFLICT)" "4" "$rc"

# 10. pr-opened DISPATCHED → ATTESTED
out="$("$OVERLAY" pr-opened test-roundtrip 7888 https://github.com/jleechanorg/worldarchitect.ai/pull/7888)"
assert "pr-opened ok" "ok" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-roundtrip';")"
assert "state after pr-opened" "ATTESTED" "$state"
pr="$(sqlite3 "$AFD_DB" "SELECT pr_number FROM bead_overlay WHERE bead_id='test-roundtrip';")"
assert "pr_number after pr-opened" "7888" "$pr"

# 11. gate-assessment ATTESTED, all-green (8 required gates + optional code_standards/zfc)
gates='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","vacuous_red_green":"green","code_standards":"green","zfc":"green"}'
out="$("$OVERLAY" gate-assessment test-roundtrip 7888 "$gates")"
assert "gate-assessment all-green → true" "true" "$(echo "$out" | head -1)"
assert "gate-assessment cooldown=false" "cooldown_ready=false" "$(echo "$out" | tail -1)"

# 11a. gate-assessment ATTESTED, 8-gate schema (code_standards/zfc now optional)
# Pull the bead back to ATTESTED to re-run gate-assessment (was READY).
"$OVERLAY" intake-upsert test-9gates 'nine gate test' >/dev/null
"$OVERLAY" route-record test-9gates STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-9gates fix/nine-gates >/dev/null
"$OVERLAY" pr-opened test-9gates 7889 https://github.com/jleechanorg/worldarchitect.ai/pull/7889 >/dev/null
gates9='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","vacuous_red_green":"green","code_standards":"green","zfc":"green"}'
out9="$("$OVERLAY" gate-assessment test-9gates 7889 "$gates9")"
assert "gate-assessment 8-gate+optional schema → true" "true" "$(echo "$out9" | head -1)"

# 11b. gate-assessment with optional zfc=red → false (optional keys still count in all_green when present)
"$OVERLAY" intake-upsert test-zfc-red 'zfc red test' >/dev/null
"$OVERLAY" route-record test-zfc-red STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-zfc-red fix/zfc-red >/dev/null
"$OVERLAY" pr-opened test-zfc-red 7890 https://github.com/jleechanorg/worldarchitect.ai/pull/7890 >/dev/null
gates_zfc_red='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","vacuous_red_green":"green","code_standards":"green","zfc":"red"}'
out_zfc="$("$OVERLAY" gate-assessment test-zfc-red 7890 "$gates_zfc_red")"
assert "gate-assessment 8-gate+optional zfc=red → false (all keys count in all_green)" "false" "$(echo "$out_zfc" | head -1)"

# 11c. gate-assessment accepts canonical 8-gate schema (see REQUIRED_KEYS in factory-overlay.sh: vacuous_red_green required since PR #413 / issue #387 r6).
"$OVERLAY" intake-upsert test-legacy 'legacy 8-gate test' >/dev/null
"$OVERLAY" route-record test-legacy STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-legacy fix/legacy-gates >/dev/null
"$OVERLAY" pr-opened test-legacy 7891 https://github.com/jleechanorg/worldarchitect.ai/pull/7891 >/dev/null
gates_legacy7='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","vacuous_red_green":"green"}'
out_legacy="$("$OVERLAY" gate-assessment test-legacy 7891 "$gates_legacy7")"
assert "gate-assessment accepts canonical 8-gate schema" "true" "$(echo "$out_legacy" | head -1)"

# 11d. gate-assessment accepts pass/warn/fail (jleechan-240 expansion).
# Warn is NON-blocking: a single warn still yields all_green=true so the
# verifier can route it through the bounded fix loop without parking
# the bead.
"$OVERLAY" intake-upsert test-pass-warn 'pass/warn enum test' >/dev/null
"$OVERLAY" route-record test-pass-warn STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-pass-warn fix/pass-warn-branch >/dev/null
"$OVERLAY" pr-opened test-pass-warn 7892 https://github.com/jleechanorg/worldarchitect.ai/pull/7892 >/dev/null
gates_pw='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":"warn"}'
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
gates_ev='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":{"verdict":"pass","evidence":[{"path":"runner/handlers.py","line":42,"msg":"no banned patterns"}]}}'
out_ev="$("$OVERLAY" gate-assessment test-evidence 7893 "$gates_ev")"
assert "gate-assessment structured-evidence → all_green true" "true" "$(echo "$out_ev" | head -1)"

# 11f. structured evidence with fail verdict → all_green=false
"$OVERLAY" intake-upsert test-ev-fail 'structured evidence fail test' >/dev/null
"$OVERLAY" route-record test-ev-fail STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-ev-fail fix/ev-fail-branch >/dev/null
"$OVERLAY" pr-opened test-ev-fail 7894 https://github.com/jleechanorg/worldarchitect.ai/pull/7894 >/dev/null
gates_evf='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":{"verdict":"fail","evidence":[{"path":"daemon/factory-overlay.sh","line":201,"msg":"keyword blacklist enforced in app code"}]}}'
out_evf="$("$OVERLAY" gate-assessment test-ev-fail 7894 "$gates_evf")"
assert "gate-assessment fail+evidence → all_green false" "false" "$(echo "$out_evf" | head -1)"

# 11g. bounded fix loop (jleechan-240): a fail gate → reroll-verdict
# (reroll_worthy) → HUMAN_HELD → recover-held → QUEUED. This proves the
# new /code-standards + /zfc gates route failed verdicts through the same
# bounded fix loop as the original 8 gates (no parallel implementation).
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
gates_uk='{"ci_green":"unknown","no_conflicts":"unknown","coderabbit":"unknown","bugbot":"unknown","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","vacuous_red_green":"unknown","code_standards":"unknown","zfc":"unknown"}'
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
out="$("$OVERLAY" park test-park session_stalled)"
assert "park ok" "ok" "$out"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-park';")"
assert "state after park" "HUMAN_HELD" "$state"

# 14. recover-held HUMAN_HELD → QUEUED
sqlite3 "$AFD_DB" <<'SQL'
INSERT INTO bead_overlay (bead_id,state,attempt,branch,session_id,park_reason,updated_at)
VALUES
  ('held-permanent','HUMAN_HELD',2,'factory/held-permanent-r2','live-permanent','session_branch_mismatch','2026-07-13T00:00:00Z'),
  ('held-unknown','HUMAN_HELD',2,'factory/held-unknown-r2',NULL,'future_reason','2026-07-13T00:00:00Z'),
  ('held-null','HUMAN_HELD',2,'factory/held-null-r2',NULL,NULL,'2026-07-13T00:00:00Z'),
  ('held-retryable-live','HUMAN_HELD',2,'factory/held-retryable-live-r2','live-retryable','session_stalled','2026-07-13T00:00:00Z');
SQL
out="$("$OVERLAY" recover-held)"
assert "recover-held delegates to canonical policy" "recovered=1" "$out"
assert "retry-safe no-session row requeued" "QUEUED|2||" \
  "$(sqlite3 -separator '|' "$AFD_DB" "SELECT state,attempt,coalesce(session_id,''),coalesce(park_reason,'') FROM bead_overlay WHERE bead_id='test-park';")"
assert "permanent live-session row preserved" \
  "HUMAN_HELD|2|factory/held-permanent-r2|live-permanent|session_branch_mismatch" \
  "$(sqlite3 -separator '|' "$AFD_DB" "SELECT state,attempt,branch,session_id,park_reason FROM bead_overlay WHERE bead_id='held-permanent';")"
assert "unknown hold preserved" \
  "HUMAN_HELD|2|factory/held-unknown-r2||future_reason" \
  "$(sqlite3 -separator '|' "$AFD_DB" "SELECT state,attempt,branch,coalesce(session_id,''),park_reason FROM bead_overlay WHERE bead_id='held-unknown';")"
assert "NULL-reason hold preserved" \
  "HUMAN_HELD|2|factory/held-null-r2||" \
  "$(sqlite3 -separator '|' "$AFD_DB" "SELECT state,attempt,branch,coalesce(session_id,''),coalesce(park_reason,'') FROM bead_overlay WHERE bead_id='held-null';")"
assert "retry-safe reason with live session preserved" \
  "HUMAN_HELD|2|factory/held-retryable-live-r2|live-retryable|session_stalled" \
  "$(sqlite3 -separator '|' "$AFD_DB" "SELECT state,attempt,branch,session_id,park_reason FROM bead_overlay WHERE bead_id='held-retryable-live';")"

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
gates='{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","vacuous_red_green":"green","code_standards":"green","zfc":"green"}'
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
gates_fail='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":"fail"}'
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
gates_evfail='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":{"verdict":"fail","evidence":[{"path":"daemon/factory-overlay.sh","line":201,"msg":"keyword in app code"}]}}'
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

# 18b. redrive-pr validation gap (rev-sp88, PR 7860 / jleechan-70lx): entry
# currently does NOT validate pr_number/branch at all (contrast with
# dispatch-record's guard at the "CR-6" test above), so an invalid branch is
# silently written into bead_overlay instead of being rejected immediately --
# the bead only fails LATER when something else (dispatch-record) tries to
# validate it, producing a silent QUEUED-but-broken churn loop instead of an
# immediate, clear rejection at the point of input.
set +e
out_badbranch="$("$OVERLAY" redrive-pr test-redrive-badbranch 9998 'factory/not-a-valid-suffix' 2>&1)"
rc_badbranch=$?
set -e
assert "redrive-pr rejects invalid branch at entry (rc=6 EX_VALID_INPUT)" "6" "$rc_badbranch"

# 18c. valid_branch's existing-PR-branch regex excludes '+' (rev-sp88 / PR
# 7860): jleechan-70lx's real branch worktree-feat+restore-rag-shadow-mode is
# rejected by this pattern, which is why that bead is stuck HUMAN_HELD.
# Exercise it through dispatch-record -- an existing valid_branch consumer --
# so the assertion is tied directly to the regex, independent of the
# redrive-pr entry-validation gap covered by 18b above.
"$OVERLAY" intake-upsert test-redrive-plus-src 'plus branch regex test' >/dev/null
"$OVERLAY" route-record test-redrive-plus-src STANDARD_PATH >/dev/null
set +e
out_plus_dispatch="$("$OVERLAY" dispatch-record test-redrive-plus-src 'worktree-feat+restore-rag-shadow-mode' 2>&1)"
rc_plus_dispatch=$?
set -e
assert "dispatch-record accepts '+' in branch name" "ok" "$out_plus_dispatch"

# 18d. redrive-pr end-to-end with the real PR 7860 branch name -- the exact
# operator command that will be run on jleechan-70lx once this fix lands.
out_plus_redrive="$("$OVERLAY" redrive-pr test-redrive-plus 7860 'worktree-feat+restore-rag-shadow-mode')"
assert "redrive-pr accepts '+' in branch name" "redriven test-redrive-plus PR #7860" "$out_plus_redrive"
state_plus_redrive="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-redrive-plus';")"
assert "state after '+' branch redrive-pr" "QUEUED" "$state_plus_redrive"

# 18e. redrive-pr rejects INVALID pr_number at entry with the documented exit
# code (bead rev-wkz63, test-hardening for PR #619 / rev-sp88). The comment
# block at the top of this file documents rc=6 (EX_VALID_INPUT) for
# "valid_branch / valid_pr (input format invalid)", but valid_pr's original
# implementation called the generic die() (rc=1), never die_code. Each case
# below also asserts NO bead_overlay row is created for the rejected bead_id
# -- proving the invalid pr_number never reaches the INSERT/UPDATE.
set +e
out_nonnumeric="$("$OVERLAY" redrive-pr test-redrive-badpr-nonnumeric abc fix/badpr-nonnumeric 2>&1)"
rc_nonnumeric=$?
set -e
assert "redrive-pr rejects non-numeric pr_number (rc=6 EX_VALID_INPUT)" "6" "$rc_nonnumeric"
count_nonnumeric="$(sqlite3 "$AFD_DB" "SELECT COUNT(*) FROM bead_overlay WHERE bead_id='test-redrive-badpr-nonnumeric';")"
assert "no bead_overlay mutation for non-numeric pr_number" "0" "$count_nonnumeric"

set +e
out_empty="$("$OVERLAY" redrive-pr test-redrive-badpr-empty "" fix/badpr-empty 2>&1)"
rc_empty=$?
set -e
assert "redrive-pr rejects empty pr_number (rc=6 EX_VALID_INPUT)" "6" "$rc_empty"
count_empty="$(sqlite3 "$AFD_DB" "SELECT COUNT(*) FROM bead_overlay WHERE bead_id='test-redrive-badpr-empty';")"
assert "no bead_overlay mutation for empty pr_number" "0" "$count_empty"

set +e
out_negative="$("$OVERLAY" redrive-pr test-redrive-badpr-negative -5 fix/badpr-negative 2>&1)"
rc_negative=$?
set -e
assert "redrive-pr rejects negative pr_number (rc=6 EX_VALID_INPUT)" "6" "$rc_negative"
count_negative="$(sqlite3 "$AFD_DB" "SELECT COUNT(*) FROM bead_overlay WHERE bead_id='test-redrive-badpr-negative';")"
assert "no bead_overlay mutation for negative pr_number" "0" "$count_negative"

# Huge pr_number (30 digits): SQLite stores integer literals too large for
# int64 as a lossy REAL (e.g. 1.0e+30), silently corrupting pr_number if this
# were ever accepted. valid_pr must reject it before it reaches the SQL layer.
set +e
out_huge="$("$OVERLAY" redrive-pr test-redrive-badpr-huge 999999999999999999999999999999 fix/badpr-huge 2>&1)"
rc_huge=$?
set -e
assert "redrive-pr rejects huge/overflow pr_number (rc=6 EX_VALID_INPUT)" "6" "$rc_huge"
count_huge="$(sqlite3 "$AFD_DB" "SELECT COUNT(*) FROM bead_overlay WHERE bead_id='test-redrive-badpr-huge';")"
assert "no bead_overlay mutation for huge pr_number" "0" "$count_huge"

# SQL-injection-shaped pr_number: valid_pr's ^[0-9]+$ regex already rejects
# any non-digit character, so this never reaches the unquoted $pr SQL
# literal. Assert rejection AND that bead_overlay survives untouched.
set +e
out_sqli="$("$OVERLAY" redrive-pr test-redrive-badpr-sqli '1; DROP TABLE bead_overlay' fix/badpr-sqli 2>&1)"
rc_sqli=$?
set -e
assert "redrive-pr rejects SQL-injection-shaped pr_number (rc=6 EX_VALID_INPUT)" "6" "$rc_sqli"
count_sqli="$(sqlite3 "$AFD_DB" "SELECT COUNT(*) FROM bead_overlay WHERE bead_id='test-redrive-badpr-sqli';")"
assert "no bead_overlay mutation for SQL-injection-shaped pr_number" "0" "$count_sqli"
table_intact="$(sqlite3 "$AFD_DB" "SELECT name FROM sqlite_master WHERE type='table' AND name='bead_overlay';")"
assert "bead_overlay table survives SQL-injection-shaped pr_number attempt" "bead_overlay" "$table_intact"

# "0" is syntactically numeric but not a valid GitHub PR number (PR numbers
# start at 1). Prior to this fix valid_pr's bare ^[0-9]+$ regex accepted it.
set +e
out_zero="$("$OVERLAY" redrive-pr test-redrive-badpr-zero 0 fix/badpr-zero 2>&1)"
rc_zero=$?
set -e
assert "redrive-pr rejects pr_number '0' (rc=6 EX_VALID_INPUT)" "6" "$rc_zero"
count_zero="$(sqlite3 "$AFD_DB" "SELECT COUNT(*) FROM bead_overlay WHERE bead_id='test-redrive-badpr-zero';")"
assert "no bead_overlay mutation for pr_number '0'" "0" "$count_zero"

# "00" / "000" (/advice REQUEST_CHANGES on #624, confirmed low-severity
# finding): the "0" rejection above used `[ "$1" != "0" ]`, a STRING
# comparison -- "00" and "000" are not string-equal to "0", so they slipped
# past that check (and the digit regex + length bound both still accept
# them), then normalized to pr_number=0 downstream in SQLite. Rejection must
# be arithmetic, not string-based.
set +e
out_zero00="$("$OVERLAY" redrive-pr test-redrive-badpr-zero00 00 fix/badpr-zero00 2>&1)"
rc_zero00=$?
set -e
assert "redrive-pr rejects pr_number '00' (rc=6 EX_VALID_INPUT)" "6" "$rc_zero00"
count_zero00="$(sqlite3 "$AFD_DB" "SELECT COUNT(*) FROM bead_overlay WHERE bead_id='test-redrive-badpr-zero00';")"
assert "no bead_overlay mutation for pr_number '00'" "0" "$count_zero00"

set +e
out_zero000="$("$OVERLAY" redrive-pr test-redrive-badpr-zero000 000 fix/badpr-zero000 2>&1)"
rc_zero000=$?
set -e
assert "redrive-pr rejects pr_number '000' (rc=6 EX_VALID_INPUT)" "6" "$rc_zero000"
count_zero000="$(sqlite3 "$AFD_DB" "SELECT COUNT(*) FROM bead_overlay WHERE bead_id='test-redrive-badpr-zero000';")"
assert "no bead_overlay mutation for pr_number '000'" "0" "$count_zero000"

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

# 22. dispatch-record capacity IO failure → EX_IO (CR-7, capacity leg).
# Simulate a broken `capacity` lookup (corrupt/missing schema) via a fake
# `sqlite3` on PATH that fails ONLY the capacity subcommand's
# `state IN ('DISPATCHED','ATTESTED')` query, and passes every other query
# (existence check, state lookup, branch_registry, INSERT) through to the
# real sqlite3 unchanged. This isolates the capacity leg specifically, the
# same way the BR_BIN shim above isolates `br show` for bead-closed-check.
FAKE_SQLITE_DIR="/tmp/test-overlay-$$-fakebin"
mkdir -p "$FAKE_SQLITE_DIR"
REAL_SQLITE3="$(command -v sqlite3)"
cat > "$FAKE_SQLITE_DIR/sqlite3" <<EOF
#!/usr/bin/env bash
for arg in "\$@"; do
  case "\$arg" in
    *"state IN ('DISPATCHED','ATTESTED')"*)
      echo "fake-sqlite3: simulated disk I/O error" >&2
      exit 10
      ;;
  esac
done
exec "$REAL_SQLITE3" "\$@"
EOF
chmod +x "$FAKE_SQLITE_DIR/sqlite3"

"$OVERLAY" intake-upsert test-cap-iofail 'capacity IO failure test' >/dev/null
"$OVERLAY" route-record test-cap-iofail STANDARD_PATH >/dev/null
set +e
out_capio="$(PATH="$FAKE_SQLITE_DIR:$PATH" "$OVERLAY" dispatch-record test-cap-iofail fix/cap-iofail-branch 2>&1)"
rc_capio=$?
set -e
assert "dispatch-record capacity IO failure -> rc=9 EX_IO" "9" "$rc_capio"
[[ "$out_capio" == *"capacity lookup failed"* ]] && echo "PASS: capacity IO failure message mentions capacity lookup" && PASS=$((PASS+1)) \
  || { echo "FAIL: capacity IO failure message did not mention capacity lookup (got: $out_capio)"; FAIL=$((FAIL+1)); }
# bead must remain QUEUED (no partial state mutation from the aborted dispatch)
state_capio="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-cap-iofail';")"
assert "capacity IO failure leaves bead in QUEUED (no partial mutation)" "QUEUED" "$state_capio"
rm -rf "$FAKE_SQLITE_DIR"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
