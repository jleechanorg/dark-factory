#!/usr/bin/env bash
# test_auto_merge_guard_gate_vocabulary.sh — focused tests for the gate
# verdict predicate that lives inside daemon/scripts/auto-merge-guard.sh
# (`latest_assessment_strict_all_green`).
#
# Background (jleechan-bze8.1 / issue #328): the auto-merge-guard consumer
# previously checked only the literal "red" token against each gate
# value. The new factory-overlay.sh gate vocabulary accepts:
#   * plain strings:  "pass" | "warn" | "fail" | "unknown"
#   * structured objects:  {"verdict": ..., "evidence": [...]}
#   * legacy aliases:  "green" -> pass, "red" -> fail
#
# These tests prove the python predicate recognizes all of the above and
# blocks merging on any `fail` verdict (including the structured shape)
# AND on any `unknown` verdict WITHOUT an operator disposition record
# (the strict-all-green rule from #328 / bze8.1). `warn` is treated as
# `pass` (non-blocking). Operator disposition is opt-in via the
# `OPERATOR_DISPOSITION:` token in the `operator_disposition` field of
# the GATE_ASSESSMENT context.
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

# emit a synthetic GATE_ASSESSMENT line that the guard can grep against
emit_assessment() { # <gates_json> [<operator_disposition>]
  local gates="$1" disp="${2:-}"
  if [ -n "$disp" ]; then
    printf '{"timestamp":"2026-07-07T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"x","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"all_green":true,"operator_disposition":"%s"}}\n' "$PR_NUM" "$gates" "$disp" > "$LOG"
  else
    printf '{"timestamp":"2026-07-07T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"x","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"all_green":true}}\n' "$PR_NUM" "$gates" > "$LOG"
  fi
}

# Extract the predicate as a standalone command via. Reuses the exact
# python heredoc block that lives in the production script — extracted
# line-by-line from the `import json` marker up to (but not including)
# the final `sys.exit(0)'` close-quote of the python heredoc.
predicate_block="$(awk '
  /^import json, sys$/ { capture=1; print; next }
  /^sys.exit\(0\)'"'"'$/ { capture=0; next }
  capture { print }
' "$GUARD")"

run_predicate() { # <input_json>
  printf '%s' "$1" | python3 -c "$predicate_block"
}

echo "=== auto-merge-guard: gate vocabulary predicate (strict-all-green) ==="

# 1. Legacy 7-gate schema with all-green -> exit 0 (strict-all-green)
emit_assessment '{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "legacy 7-gate all-green -> exit 0 (strict-all-green)" "0" "$rc"
case "$out" in
  *strict-all-green*) echo "PASS: legacy 7-gate all-green -> strict-all-green message"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: legacy 7-gate all-green -> unexpected output: $out"; FAIL=$((FAIL+1)) ;;
esac

# 2. 7 required gates + optional code_standards/zfc all-pass -> exit 0
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "7-gate+optional all-pass -> exit 0" "0" "$rc"

# 3. Optional zfc: "fail" still blocks merging (guard checks ALL gates present)
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"fail"}'
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
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":{"verdict":"fail","evidence":[{"path":"x","line":1,"msg":"y"}]}}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "structured {verdict:fail} -> exit 1 (BLOCK)" "1" "$rc"

# 5. warn verdict stays non-blocking (treated as pass under strict-all-green)
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"warn","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "warn verdict -> exit 0 (non-blocking, treated as pass)" "0" "$rc"

# 6. NEW (bze8.1 / #328): unknown verdict WITHOUT operator disposition -> BLOCKS (exit 1).
# This is the regression-class fix: pre-fix, the auto-merge-guard permitted
# unknowns and merged #365/#375/#382 with structural-pending unknowns.
emit_assessment '{"ci_green":"unknown","no_conflicts":"unknown","coderabbit":"unknown","bugbot":"unknown","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","code_standards":"unknown","zfc":"unknown"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "all-unknown WITHOUT disposition -> exit 1 (ESCALATION_REQUIRED)" "1" "$rc"
case "$out" in
  *ESCALATION_REQUIRED*) echo "PASS: all-unknown emits ESCALATION_REQUIRED message"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: all-unknown -> unexpected output: $out"; FAIL=$((FAIL+1)) ;;
esac

# 7. NEW (bze8.1 / #328): unknown verdict WITH operator disposition -> proceeds (exit 0).
# The operator's explicit override is the ONLY authorized bypass.
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"unknown","bugbot":"unknown","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}' "OPERATOR_DISPOSITION: CodeRabbit+Bugbot quota wall; manual review confirmed"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "unknown WITH operator disposition -> exit 0 (override)" "0" "$rc"
case "$out" in
  *unknowns-overridden*) echo "PASS: disposition override emits the right message"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: disposition override -> unexpected output: $out"; FAIL=$((FAIL+1)) ;;
esac

# 8. NEW (bze8.1 / #328): unknown WITHOUT disposition -> BLOCKS even with all other gates pass.
# This is the regression-class test that exercises the #365/#375/#382 path:
# every gate is Green except CodeRabbit (Unknown) and Bugbot (Unknown) —
# pre-fix, this would merge. Post-fix, it must block.
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"unknown","bugbot":"unknown","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "two-unknown WITHOUT disposition -> exit 1 (BLOCK — regression-class fix)" "1" "$rc"

# 9. fail mixed with warn + unknown still blocks (any fail is enough)
emit_assessment '{"ci_green":"pass","no_conflicts":"warn","coderabbit":"unknown","bugbot":"pass","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","code_standards":"pass","zfc":"fail"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "fail + warn + unknown mix -> exit 1 (any-fail blocks)" "1" "$rc"

# 10. Legacy "red" alias -> still blocks (back-compat)
emit_assessment '{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"red","comments_resolved":"green","evidence_review":"green","skeptic":"green","code_standards":"green","zfc":"green"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "legacy red alias -> exit 1 (BLOCK, back-compat)" "1" "$rc"

# 11. empty / unparseable input -> exit 1 (block on missing)
set +e
out="$(run_predicate 'not-json')"
rc=$?
set -e
assert "unparseable input -> exit 1 (block on missing)" "1" "$rc"

# 12. empty operator_disposition string -> still BLOCKS unknowns (no token)
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"unknown","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}' ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "empty operator_disposition -> exit 1 (no token, blocks unknowns)" "1" "$rc"

# 13. operator_disposition without the literal "OPERATOR_DISPOSITION:" token -> still BLOCKS
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"unknown","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}' "manual override by jleechan 2026-07-19"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "operator_disposition WITHOUT OPERATOR_DISPOSITION: token -> exit 1 (blocks unknowns)" "1" "$rc"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
