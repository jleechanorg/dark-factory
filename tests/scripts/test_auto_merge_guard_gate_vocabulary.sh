#!/usr/bin/env bash
# test_auto_merge_guard_gate_vocabulary.sh — focused tests for the gate
# verdict predicate that lives inside daemon/scripts/auto-merge-guard.sh
# (`latest_assessment_no_red`).
#
# Background (jleechan-240 review thread): the auto-merge-guard consumer
# previously checked only the literal "red" token against each gate
# value. The new factory-overlay.sh gate vocabulary accepts:
#   * plain strings:  "pass" | "warn" | "fail" | "unknown"
#   * structured objects:  {"verdict": ..., "evidence": [...]}
#   * legacy aliases:  "green" -> pass, "red" -> fail
#
# These tests prove the python predicate recognizes all of the above and
# blocks merging on any `fail` verdict (including the structured shape),
# while keeping `warn` and `unknown` non-blocking per the documented
# no-red merge policy. Each test pipes a synthetic GATE_ASSESSMENT
# JSONL entry into the same python heredoc that the production script
# uses, so behavior matches verbatim.
#
# Run with: bash tests/scripts/test_auto_merge_guard_gate_vocabulary.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"
LOG="/tmp/test-guard-vocab-$$.jsonl"
PR_NUM=99001
trap 'rm -f "$LOG"' EXIT

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"; FAIL=$((FAIL + 1))
  fi
}

# Synthetic head SHA carried in every synthetic GATE_ASSESSMENT line so
# the predicate's P1 #1 exact-head binding passes (PR #328 / bze8.1).
LIVE_HEAD="0123456789abcdef0123456789abcdef01234567"

# emit a synthetic GATE_ASSESSMENT line that the guard can grep against
emit_assessment() { # <gates_json>
  printf '{"timestamp":"2026-07-07T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"x","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"all_green":true,"head_sha":"%s"}}\n' "$PR_NUM" "$1" "$LIVE_HEAD" > "$LOG"
}

# Extract the predicate as a standalone command via. Reuses the exact
# python heredoc block that lives at lines 85-158 of the production
# script (PR #328 / bze8.1 added head_sha / canonical gate-key set /
# operator_disposition; line 84 is the bash `printf ... | python3 -c '`
# wrapper and line 159 is the trailing `'"$live_head"` close-arg, neither
# of which Python accepts inside `-c`).
# jleechan-ni1k / issue #437 P2 widened the predicate to 8 required gates
# (added `vacuous_red_green`) and added the P1 LIVE_HEAD_MISSING branch;
# the block spanned 41..=114 pre-rev-1uno. Bead rev-1uno inserted a
# rate_limit quota preflight (44 lines) ahead of this function, shifting
# the range to 85..=158 — keep this range in lockstep with any future
# edits above `latest_assessment_no_red()` in auto-merge-guard.sh.
predicate_block="$(sed -n '120,193p' "$GUARD" | sed 's/^  //')"

run_predicate() { # <input_json>
  printf '%s' "$1" | python3 -c "$predicate_block" "$LIVE_HEAD"
}

echo "=== auto-merge-guard: gate vocabulary predicate ==="

# 1. Legacy 7-gate schema with all-green -> all_pass (exit 0)
emit_assessment '{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green","vacuous_red_green":"green"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "legacy 8-gate all-green -> exit 0" "0" "$rc"
case "$out" in
  *no-fail*) echo "PASS: legacy 8-gate all-green -> no-fail message"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: legacy 8-gate all-green -> unexpected output: $out"; FAIL=$((FAIL+1)) ;;
esac

# 2. 8 required gates + optional code_standards/zfc all-pass -> no-fail (exit 0)
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":"pass"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "8-gate+optional all-pass -> exit 0" "0" "$rc"

# 3. Optional zfc: "fail" still blocks merging (guard checks ALL gates present)
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":"fail"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "optional zfc=fail -> exit 1 (BLOCK)" "1" "$rc"
case "$out" in
  FAIL:*zfc*) echo "PASS: zfc=fail emits FAIL:zfc message"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: zfc=fail -> unexpected output: $out"; FAIL=$((FAIL+1)) ;;
esac

# 4. NEW: structured object {"verdict":"fail"} -> blocks merging (exit 1)
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":{"verdict":"fail","evidence":[{"path":"x","line":1,"msg":"y"}]}}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "structured {verdict:fail} -> exit 1 (BLOCK)" "1" "$rc"

# 5. warn verdict stays non-blocking (per documented no-red policy)
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"warn","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass","code_standards":"pass","zfc":"pass"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "warn verdict -> exit 0 (non-blocking)" "0" "$rc"
case "$out" in
  *warn*) echo "PASS: warn verdict listed in no-fail message"; PASS=$((PASS+1)) ;;
  *) echo "note: warn verdict may or may not be listed; output: $out" ;;
esac

# 6. unknown verdict stays non-blocking (defers to next tick)
emit_assessment '{"ci_green":"unknown","no_conflicts":"unknown","coderabbit":"unknown","bugbot":"unknown","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","vacuous_red_green":"unknown","code_standards":"unknown","zfc":"unknown"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "all-unknown -> exit 0 (defers to next tick)" "0" "$rc"

# 7. Legacy "red" alias -> still blocks (back-compat)
emit_assessment '{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"red","comments_resolved":"green","evidence_review":"green","skeptic":"green","vacuous_red_green":"green","code_standards":"green","zfc":"green"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "legacy red alias -> exit 1 (BLOCK, back-compat)" "1" "$rc"

# 8. fail mixed with warn + unknown still blocks (any fail is enough)
emit_assessment '{"ci_green":"pass","no_conflicts":"warn","coderabbit":"unknown","bugbot":"pass","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","vacuous_red_green":"pass","code_standards":"pass","zfc":"fail"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "fail + warn + unknown mix -> exit 1 (any-fail blocks)" "1" "$rc"

# 9. empty / unparseable input -> exit 1 (block on missing)
set +e
out="$(run_predicate 'not-json')"
rc=$?
set -e
assert "unparseable input -> exit 1 (block on missing)" "1" "$rc"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
