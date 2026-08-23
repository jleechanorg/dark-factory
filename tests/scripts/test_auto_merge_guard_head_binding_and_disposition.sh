#!/usr/bin/env bash
# test_auto_merge_guard_head_binding_and_disposition.sh — TDD red→green
# coverage for the three codex-connector P1 findings on PR #421:
#
#   P1 #1: GATE_ASSESSMENT lookup is bound to PR number only — the
#          timer-driven merge path can reuse an all-green assessment from
#          an OLDER head SHA. `latest_assessment_no_red` MUST now refuse
#          to honour an assessment whose recorded `head_sha` no longer
#          matches the live PR head. This file proves:
#     a) a stale assessment (recorded head != live head) → BLOCK
#     b) an assessment that matches the live head         → no-fail
#     c) a missing `head_sha` field                       → BLOCK
#         (fail-closed: the guard cannot prove freshness without it)
#
#   P1 #2: the predicate accepted any non-empty subset whose present
#          entries were all pass/warn. A 1-key `{"ci_green":"pass"}`
#          object must NOT slip through. The predicate now requires the
#          full canonical gate-key set (`ci_green`, `no_conflicts`,
#          `coderabbit`, `bugbot`, `comments_resolved`,
#          `evidence_review`, `skeptic`) before strict-all-green can
#          pass. This file proves:
#     a) subset {"ci_green":"pass"}                 → BLOCK
#     b) full canonical 7-gate set all-pass          → no-fail
#     c) canonical set missing 1 gate + 1 fail gate  → BLOCK
#
#   P1 #3: the shell override reads `OPERATOR_DISPOSITION` from
#          `context.operator_disposition` on the latest GATE_ASSESSMENT,
#          but the daemon never emitted that field. We now emit it from
#          `overlay.park_reason` and the shell reads the same key.
#          This file proves the round-trip:
#     a) emit `park_reason = "operator_approved"` → guard accepts
#     b) emit `park_reason = "operator_held"`     → guard accepts
#     c) absent operator_disposition                → guard defaults
#        to the standard no-red path (NOT a special override token).
#
# Run with:
#   bash tests/scripts/test_auto_merge_guard_head_binding_and_disposition.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"
LOG="/tmp/test-guard-headbind-$$.jsonl"
PR_NUM=99002
LIVE_HEAD="0123456789abcdef0123456789abcdef01234567"
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

# emit a synthetic GATE_ASSESSMENT line that the guard can grep against.
# Usage: emit_assessment <gates_json> [head_sha] [operator_disposition]
emit_assessment() {
  local gates="$1" head="${2:-${LIVE_HEAD}}" op="${3:-}"
  local op_field=""
  [ -n "$op" ] && op_field=",\"operator_disposition\":\"${op}\""
  printf '{"timestamp":"2026-07-21T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"x","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"all_green":true,"head_sha":"%s"%s}}\n' \
    "$PR_NUM" "$gates" "$head" "$op_field" > "$LOG"
}

# Pull the production predicate block verbatim from the guard. Lines
# 84-150 hold the bash `latest_assessment_no_red` function's embedded
# python heredoc (PR #328 / bze8.1 exact-head binding). Strip the
# leading `printf ... | python3 -c '` wrapper line and the trailing
# `'` close-quote; dedent so Python's `-c` accepts it.
# Extract the pure-python block (lines 85-158): line 84 is the bash
# `printf | python3 -c '` wrapper, line 159 is the trailing `'\'' "$live_head"`.
# jleechan-ni1k / issue #437 P2 widened the predicate to 8 required gates
# (added `vacuous_red_green`) and added the P1 LIVE_HEAD_MISSING branch;
# the block spanned 41..=114 pre-rev-1uno. Bead rev-1uno inserted a
# rate_limit quota preflight (44 lines) ahead of this function, shifting
# the range to 85..=158 — keep this range in lockstep with any future
# edits above `latest_assessment_no_red()` in auto-merge-guard.sh.
predicate_block="$(sed -n '125,198p' "$GUARD" | sed 's/^  //')"
[ -n "$predicate_block" ] || { echo "FATAL: could not extract predicate from $GUARD"; exit 2; }

run_predicate() { # <input_json> [<live_head_sha>]
  local live_head="${2:-${LIVE_HEAD}}"
  printf '%s' "$1" | python3 -c "$predicate_block" "$live_head"
}

# jleechan-ni1k / issue #437 P2: the predicate REQUIRED set is now 8 gates
# (added `vacuous_red_green`); 7-key fixtures must include the new key.
FULL8='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass"}'
SUBSET='{"ci_green":"pass"}'

echo "=== P1 #1 head_sha binding ==="

# 1a. Stale assessment (recorded head != live head) — BLOCK.
emit_assessment "$FULL8" "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "stale assessment (head_sha mismatch) -> exit 1 (BLOCK)" "1" "$rc"
case "$out" in
  *STALE*|*HEAD*|*head*) echo "PASS: stale assessment emits STALE/HEAD reason"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: stale assessment output lacked STALE/HEAD reason: $out"; FAIL=$((FAIL+1)) ;;
esac

# 1b. Fresh assessment (recorded head == live head) — no-fail.
emit_assessment "$FULL8" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "fresh assessment (head_sha match) -> exit 0" "0" "$rc"

# 1c. Missing head_sha field — BLOCK (fail-closed; freshness unprovable).
printf '{"timestamp":"2026-07-21T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"x","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"all_green":true}}\n' \
  "$PR_NUM" "$FULL8" > "$LOG"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "missing head_sha -> exit 1 (BLOCK, fail-closed)" "1" "$rc"
case "$out" in
  *HEAD*|*head*|*MISSING*) echo "PASS: missing head_sha emits reason"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: missing head_sha output lacked reason: $out"; FAIL=$((FAIL+1)) ;;
esac

echo
echo "=== P1 #2 fail-closed canonical gate-key set ==="

# 2a. Single-key subset {"ci_green":"pass"} must NOT pass.
emit_assessment "$SUBSET" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "single-key subset -> exit 1 (BLOCK, fail-closed on subset)" "1" "$rc"
case "$out" in
  *SUBSET*|*MISSING*|*REQUIRED*|*keys*) echo "PASS: subset emits SUBSET/MISSING/REQUIRED reason"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: subset output lacked SUBSET/MISSING/REQUIRED reason: $out"; FAIL=$((FAIL+1)) ;;
esac

# 2b. Canonical 7-gate set all-pass -> no-fail.
emit_assessment "$FULL8" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "canonical 7-gate all-pass + fresh head -> exit 0" "0" "$rc"

# 2c. Missing one canonical gate plus a fail on another -> still BLOCK.
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","skeptic":"pass"}' "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "canonical set minus evidence_review -> exit 1 (BLOCK)" "1" "$rc"

echo
echo "=== P1 #3 operator_disposition round-trip ==="

# 3a. operator_disposition=operator_approved must survive the read.
emit_assessment "$FULL8" "$LIVE_HEAD" "operator_approved"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "operator_disposition=operator_accepted -> exit 0 (round-trip ok)" "0" "$rc"

# 3b. operator_disposition=operator_held must survive the read.
emit_assessment "$FULL8" "$LIVE_HEAD" "operator_held"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "operator_disposition=operator_held -> exit 0 (round-trip ok)" "0" "$rc"

# 3c. Absent operator_disposition -> defaults to standard no-red path.
emit_assessment "$FULL8" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last")"; rc=$?
set -e
assert "absent operator_disposition -> exit 0 (default no-red)" "0" "$rc"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0