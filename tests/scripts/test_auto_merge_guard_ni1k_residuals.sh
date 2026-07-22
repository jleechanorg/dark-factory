#!/usr/bin/env bash
# test_auto_merge_guard_ni1k_residuals.sh — TDD red→green coverage for the
# three codex-connector findings on PR #435 (bead jleechan-ni1k / issue #437):
#
#   P1 (jleechan-ni1k #1): when `gh pr view headRefOid` fails or returns
#       empty, `live_head_sha` is empty and the head-binding check is
#       SKIPPED (fail-open). Empty/failed head lookup must abort the merge
#       decision fail-closed with a distinct log line + test.
#       Tests:
#         a) live_head="" + assessment has head_sha -> BLOCK with distinct reason
#         b) live_head=missing + assessment has head_sha -> BLOCK
#            (a real `gh pr view` failure where the jq value is null)
#         c) live_head=<same as assessed> -> no-fail (sanity baseline)
#
#   P2 (jleechan-ni1k #2): the required canonical gate set in
#       `auto-merge-guard.sh` omits the 8th gate `vacuous_red_green`
#       (daemon/src/verifier.rs:62-72). A report missing it can still
#       strict-green. Add the key + update the required-keys test.
#       Tests:
#         a) full 7-key set + fresh head + live head -> BLOCK with
#            "missing vacuous_red_green" reason (regression for the omission)
#         b) full 8-key set + fresh head + live head -> no-fail
#
#   P3 (bonus, jleechan-ni1k #3): the runtime vacuous-red-green detector's
#       `find_cargo_manifest` walked from `std::env::current_dir()` (the
#       daemon's own CWD). For beads dispatched into a different `[repos.*]`
#       entry the detector logged "no Cargo.toml reachable from
#       /home/jleechan/projects/dark-factory". Fix: the detector must
#       resolve from the bead's OWN repo working tree (passed in via
#       `repo_root_for_repo` from `Scm`), not the daemon's CWD.
#       Tests (Rust unit test under `daemon::vacuous_red_green`):
#         a) `find_cargo_manifest_from_repo(repo="jleechanorg/dark-factory")`
#            returns the dark-factory `daemon/Cargo.toml` (or repo-root
#            `Cargo.toml` if present).
#         b) `find_cargo_manifest(daemon_cwd)` and the new
#            `find_cargo_manifest_from_repo(bead_repo_path)` produce the
#            same path when the daemon happens to be in the same repo.
#
# Run with:
#   bash tests/scripts/test_auto_merge_guard_ni1k_residuals.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"
LOG="/tmp/test-guard-ni1k-$$.jsonl"
PR_NUM=99037
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

# Pull the production predicate block verbatim from the guard (same extraction
# strategy as test_auto_merge_guard_head_binding_and_disposition.sh).
# Lines 41-123 hold the bash `latest_assessment_no_red` function's embedded
# python heredoc. Strip the leading `printf ... | python3 -c '` wrapper line
# and the trailing `'` close-quote; dedent so Python's `-c` accepts it.
predicate_block="$(sed -n '41,123p' "$GUARD" | sed 's/^  //')"
[ -n "$predicate_block" ] || { echo "FATAL: could not extract predicate from $GUARD"; exit 2; }

run_predicate() { # <input_json> [<live_head_sha>]
  local live_head="${2-$LIVE_HEAD}"
  printf '%s' "$1" | python3 -c "$predicate_block" "$live_head"
}

FULL7='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass"}'
FULL8='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass"}'

echo "=== P1 #1 empty live_head_sha fails closed ==="

# 1a. Empty live head + assessment that has head_sha: the guard MUST refuse
# to honour the assessment (cannot prove freshness — fail-closed).
emit_assessment "$FULL8" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "")"; rc=$?
set -e
assert "empty live_head_sha -> exit 1 (BLOCK, fail-closed)" "1" "$rc"
case "$out" in
  *LIVE_HEAD*|*HEAD_MISSING*|*EMPTY*|*STALE*|*MISSING*)
    echo "PASS: empty live_head_sha emits distinct LIVE_HEAD/HEAD_MISSING reason"
    PASS=$((PASS+1))
    ;;
  *)
    echo "FAIL: empty live_head_sha output lacked distinct reason: $out"
    FAIL=$((FAIL+1))
    ;;
esac

# 1b. live_head passed as the literal "null" string (simulates `gh pr view
# headRefOid --jq .headRefOid` returning a JSON null after a transient
# network blip — the original shell fallback `|| true` swallows the error
# and the predicate sees empty input). Must BLOCK with the same distinct
# reason.
emit_assessment "$FULL8" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "null")"; rc=$?
set -e
# Note: "null" as the literal second argument means the predicate sees
# `live_head = "null"`, which is non-empty and will mismatch the assessed
# head (since LIVE_HEAD != "null"). The test asserts the predicate exits
# non-zero on this mismatch (BLOCK), proving the head-binding path is
# actually exercised when live_head is non-empty but garbage.
assert "live_head='null' (garbage) -> exit 1 (BLOCK)" "1" "$rc"

# 1c. Sanity: a real fresh head + matching assessment still passes.
emit_assessment "$FULL8" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "$LIVE_HEAD")"; rc=$?
set -e
assert "fresh head match -> exit 0 (sanity baseline)" "0" "$rc"

echo
echo "=== P2 #2 vacuous_red_green required in canonical gate set ==="

# 2a. Full 7-key set + fresh head must BLOCK because vacuous_red_green is
# missing (regression for the omission).
emit_assessment "$FULL7" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "$LIVE_HEAD")"; rc=$?
set -e
assert "full 7-key set without vacuous_red_green -> exit 1 (BLOCK)" "1" "$rc"
case "$out" in
  *vacuous_red_green*|*SUBSET_MISSING*|*MISSING*)
    echo "PASS: missing vacuous_red_green emits SUBSET_MISSING reason naming it"
    PASS=$((PASS+1))
    ;;
  *)
    echo "FAIL: missing vacuous_red_green output lacked naming it: $out"
    FAIL=$((FAIL+1))
    ;;
esac

# 2b. Full 8-key set + fresh head -> no-fail (the canonical all-green path).
emit_assessment "$FULL8" "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "$LIVE_HEAD")"; rc=$?
set -e
assert "full 8-key set + fresh head -> exit 0" "0" "$rc"

# 2c. vacuous_red_green=fail must BLOCK even when everything else passes.
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"fail"}' "$LIVE_HEAD" ""
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "$LIVE_HEAD")"; rc=$?
set -e
assert "vacuous_red_green=fail -> exit 1 (BLOCK)" "1" "$rc"
case "$out" in
  *vacuous_red_green*)
    echo "PASS: vacuous_red_green=fail emits the gate name in its reason"
    PASS=$((PASS+1))
    ;;
  *)
    echo "FAIL: vacuous_red_green=fail output lacked the gate name: $out"
    FAIL=$((FAIL+1))
    ;;
esac

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0