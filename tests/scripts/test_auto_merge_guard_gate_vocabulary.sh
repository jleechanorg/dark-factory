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

# emit a synthetic GATE_ASSESSMENT line that the guard can grep against
emit_assessment() { # <gates_json>
  printf '{"timestamp":"2026-07-07T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"x","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"all_green":true}}\n' "$PR_NUM" "$1" > "$LOG"
}

# emit_assessment_with_review_degraded — same shape as `emit_assessment` but
# with `review_degraded` lifted into context alongside `gates` (where the
# daemon emits it per jleechan-984e / issue #385). Used by tests 15 & 16.
emit_assessment_with_review_degraded() { # <gates_json> <review_degraded_bool>
  printf '{"timestamp":"2026-07-07T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"x","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"review_degraded":%s,"all_green":true}}\n' "$PR_NUM" "$1" "$2" > "$LOG"
}

# Extract the predicate as a standalone command via. Reuses the exact
# python heredoc block that lives at lines 41-104 of the production
# script (line 40 is the bash `printf ... | python3 -c '` wrapper; line
# 104's trailing `'` is stripped because bash command-substitution
# would re-inject it as a Python syntax error).
predicate_block="$(sed -n '41,104p' "$GUARD" | sed "s/'$//")"

run_predicate() { # <input_json>
  printf '%s' "$1" | python3 -c "$predicate_block"
}

echo "=== auto-merge-guard: gate vocabulary predicate ==="

# 1. Legacy 7-gate schema with all-green -> all_pass (exit 0)
emit_assessment '{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"green","comments_resolved":"green","evidence_review":"green","skeptic":"green"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "legacy 7-gate all-green -> exit 0" "0" "$rc"
case "$out" in
  *no-fail*) echo "PASS: legacy 7-gate all-green -> no-fail message"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: legacy 7-gate all-green -> unexpected output: $out"; FAIL=$((FAIL+1)) ;;
esac

# 2. 7 required gates + optional code_standards/zfc all-pass -> no-fail (exit 0)
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

# 5. warn verdict stays non-blocking (per documented no-red policy)
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"warn","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}'
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

# 6. unknown verdict BLOCKS the merge (fail-closed contract, issue #328).
# Pre-#328 behavior treated unknown as a deferral ("next tick"). That was
# the exact bypass the bead names: a disposition note could substitute
# for strict 7-green. Post-#328: any unknown gate blocks the merge.
emit_assessment '{"ci_green":"unknown","no_conflicts":"unknown","coderabbit":"unknown","bugbot":"unknown","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","code_standards":"unknown","zfc":"unknown"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "all-unknown -> exit 1 (BLOCK, fail-closed)" "1" "$rc"

# 7. Legacy "red" alias -> still blocks (back-compat)
emit_assessment '{"ci_green":"green","no_conflicts":"green","coderabbit":"green","bugbot":"red","comments_resolved":"green","evidence_review":"green","skeptic":"green","code_standards":"green","zfc":"green"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "legacy red alias -> exit 1 (BLOCK, back-compat)" "1" "$rc"

# 8. fail mixed with warn + unknown still blocks (any fail is enough)
emit_assessment '{"ci_green":"pass","no_conflicts":"warn","coderabbit":"unknown","bugbot":"pass","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","code_standards":"pass","zfc":"fail"}'
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

# ============================================================================
# jleechan-org/dark-factory#328: fail-closed merge-authority contract
# ============================================================================
# The merge authority MUST fail closed when ANY gate is unknown, the
# assessment is stale, the verdict is unparseable, or the review was
# degraded (single-model family). The legacy "unknown defers to next tick"
# behavior was the exact bypass the bead names: a disposition note could
# substitute for strict 7-green. These tests pin the new strict contract.
echo
echo "=== auto-merge-guard: fail-closed contract (issue #328) ==="

# 10. ANY single unknown gate must BLOCK the merge (fail-closed).
# Pre-#328 behavior: unknown gates deferred to the next tick; a
# disposition note could substitute. Post-#328: unknown is not proven.
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"unknown","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "single unknown gate (coderabbit) -> exit 1 (BLOCK, fail-closed)" "1" "$rc"

# 11. All-unknown gates must BLOCK (the entire chain is unverified).
emit_assessment '{"ci_green":"unknown","no_conflicts":"unknown","coderabbit":"unknown","bugbot":"unknown","comments_resolved":"unknown","evidence_review":"unknown","skeptic":"unknown","code_standards":"unknown","zfc":"unknown"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "all-unknown gates -> exit 1 (BLOCK, not defer)" "1" "$rc"

# 12. Structured {verdict: unknown} object must also BLOCK.
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":{"verdict":"unknown","evidence":["quota-limited"]},"bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "structured {verdict:unknown} object -> exit 1 (BLOCK)" "1" "$rc"

# 13. Empty/missing gates object -> BLOCK (fail-closed on missing evidence).
emit_assessment '{}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "empty gates object -> exit 1 (BLOCK, fail-closed on missing)" "1" "$rc"

# 14. Gate value with an unparseable verdict token -> BLOCK
# (e.g. "approved" or "yellow-pass" — neither pass|warn|fail|unknown).
emit_assessment '{"ci_green":"approved","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "unparseable verdict token -> exit 1 (BLOCK)" "1" "$rc"

# 15. Review-degraded flag set -> BLOCK (single-model family review).
# jleechan-984e: when CodeRabbit/Bugbot/Skeptic all came from one model
# family (e.g. only claude because codex was quota-dead), the strict
# merge policy must refuse to call the PR green. The telemetry emits a
# `review_degraded: true` flag at the context level; the merge-authority
# predicate must read it and fail closed.
emit_assessment_with_review_degraded '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}' 'true'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "review_degraded=true -> exit 1 (BLOCK, strict merge policy)" "1" "$rc"

# 16. Review-degraded flag set to false -> ALLOW (multi-model family).
emit_assessment_with_review_degraded '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","code_standards":"pass","zfc":"pass"}' 'false'
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"
rc=$?
set -e
assert "review_degraded=false + all-pass -> exit 0 (ALLOW)" "0" "$rc"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
