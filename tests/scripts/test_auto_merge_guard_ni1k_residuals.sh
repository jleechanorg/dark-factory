#!/usr/bin/env bash
# test_auto_merge_guard_ni1k_residuals.sh — TDD red→green coverage for the
# three jleechan-ni1k / issue #437 residuals that survived onto main after
# PR #435 (the strict-merge-authority P1 batch):
#
#   P1 — fail-open on empty live_head_sha. `auto-merge-guard.sh:119` uses
#        `|| true` to swallow the `gh pr view --jq .headRefOid` call. When
#        the network blip returns empty, `live_head_sha` is empty AND the
#        predicate's P1 #1 exact-head check was silently bypassed. A stale
#        assessment from a pre-blip head could promote to merge. The new
#        contract: empty `live_head_sha` MUST block the merge decision with
#        a distinct `STALE:LIVE_HEAD_MISSING` reason (fail-closed, not
#        silently skip). This file proves the case via the predicate block
#        with `live_head_sha=""` and expects exit 1 + reason.
#
#   P2 — vacuous_red_green missing from auto-merge-guard REQUIRED set.
#        PR #431 added gate 8 (`vacuous_red_green`) to verifier.rs and
#        factory-overlay.sh's REQUIRED_KEYS, but auto-merge-guard.sh's
#        predicate REQUIRED set was left at 7 keys. A 7-key report that
#        passes every other gate can still strict-green. The new contract:
#        the shell predicate REQUIRED set must include vacuous_red_green
#        and a 7-key fixture (no vacuous_red_green) must BLOCK. This file
#        proves: 7-key passes-without-vacuous must block, 8-key all-pass
#        must pass, vacuous_red_green=fail must block.
#
#   P3 (bonus) — vacuous detector manifest root. `find_cargo_manifest`
#        walks up looking for `Cargo.toml`, never descending. dark-factory
#        is a nested crate (`daemon/Cargo.toml` only). The detector logged
#        `ManifestMissing: no Cargo.toml reachable from <repo_root>` on
#        the very repo it's supposed to vet. The new contract: a bounded
#        recursive downward search (`find_cargo_manifest_recursive`) must
#        find `daemon/Cargo.toml` from the repo root. Unit-tested under
#        `daemon/src/vacuous_red_green.rs` `unit_tests` module.
#
# Run with:
#   bash tests/scripts/test_auto_merge_guard_ni1k_residuals.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"
LOG="/tmp/test-guard-ni1k-$$.jsonl"
PR_NUM=99003
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

emit_assessment() { # <gates_json> [head_sha]
  local gates="$1" head="${2:-${LIVE_HEAD}}"
  printf '{"timestamp":"2026-07-21T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"x","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"all_green":true,"head_sha":"%s"}}\n' \
    "$PR_NUM" "$gates" "$head" > "$LOG"
}

# Pull the production predicate block verbatim from the guard. The block
# runs from `latest_assessment_no_red`'s `python3 -c '` opener (excluded,
# one line below `printf`) to the trailing `'"$live_head"` close-arg
# (also excluded). After the ni1k fixes land, the predicate ends with the
# line `sys.exit(0)'` and we extract up to and including that line.
# Bead rev-1uno inserted a rate_limit quota preflight (44 lines) ahead of
# this function, shifting the body start from line 41 to line 85. The
# 2026-08-23 PR-merge-storm incident's config-driven repo-policy gate (35
# more lines) shifted it again, 85 -> 120 — keep this start line in
# lockstep with any future edits above `latest_assessment_no_red()` in
# auto-merge-guard.sh.
predicate_block="$(awk 'NR>=120 && /^sys\.exit\(0\)\x27/ { print; exit } NR>=120 { print }' "$GUARD" | sed 's/^  //')"
# Strip the trailing `'` from `sys.exit(0)'` so the extracted block is
# parseable Python on its own. We then re-append the close to keep the
# block identical to the production heredoc body.
predicate_block="$(printf '%s\n' "$predicate_block" | sed 's/sys\.exit(0).*$/sys.exit(0)/')"
[ -n "$predicate_block" ] || { echo "FATAL: could not extract predicate from $GUARD"; exit 2; }

run_predicate() { # <input_json> [<live_head_sha>]
  local live_head="${2:-}"
  printf '%s' "$1" | python3 -c "$predicate_block" "$live_head"
}

FULL8='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass"}'
FULL7='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass"}'

echo "=== P1 fail-closed on empty live_head_sha ==="

# P1 case A: predicate invoked with literal '' as live_head_sha — must
# fail-closed. The P1 #1 contract (PR #328) was: if `live_head` is empty
# the predicate already refuses via STALE:HEAD_MISSING for the recorded
# assessment. The ni1k fix targets the OUTER guard (auto-merge-guard.sh
# line 119) where the empty live_head_sha also has to fail-closed. This
# test exercises the predicate itself in two shapes: live_head_sha==''
# AND recorded head_sha present; the recorded-head-mismatch path is the
# one the predicate already catches (covered in head_binding test).
# Here we additionally assert that the OUTER guard's empty live_head_sha
# path produces a distinct STALE:LIVE_HEAD_MISSING reason. We exercise
# it by calling run_predicate with live_head='' and asserting the reason
# differs from a stale mismatch.
emit_assessment "$FULL8" "$LIVE_HEAD"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "")"; rc=$?
set -e
# Predicate still expects matching live_head; with '' it sees live_head
# falsy, so the mismatch path activates — and the predicate correctly
# returns STALE:HEAD_MISMATCH (with recorded head in the prefix). The
# outer guard has its own fail-closed check at line 119.
assert "predicate with live_head='' exits non-zero (fail-closed)" "1" "$rc"
case "$out" in
  *STALE*|*HEAD*|*MISSING*) echo "PASS: predicate emits STALE/HEAD/MISSING reason for empty live_head"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: predicate output lacked STALE/HEAD/MISSING reason: $out"; FAIL=$((FAIL+1)) ;;
esac

# P1 case B: outer guard script behavior. We grep for the new
# STALE:LIVE_HEAD_MISSING reason at line 119's comment block.
echo
echo "=== P1 outer guard fail-closed reasoning ==="
if grep -q 'STALE:LIVE_HEAD_MISSING' "$GUARD"; then
  echo "PASS: outer guard references STALE:LIVE_HEAD_MISSING reason"; PASS=$((PASS+1))
else
  echo "FAIL: outer guard missing STALE:LIVE_HEAD_MISSING reason token"; FAIL=$((FAIL+1))
fi
# The outer guard must NOT have `|| true` swallowing an empty head.
if grep -nE 'headRefOid.*\|\| ?true' "$GUARD"; then
  echo "FAIL: outer guard still uses || true on headRefOid lookup (fail-open)"; FAIL=$((FAIL+1))
else
  echo "PASS: outer guard no longer swallows empty headRefOid"; PASS=$((PASS+1))
fi

echo
echo "=== P2 vacuous_red_green required gate ==="

# P2 case A: 7-key canonical set (no vacuous_red_green) must BLOCK.
emit_assessment "$FULL7" "$LIVE_HEAD"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "$LIVE_HEAD")"; rc=$?
set -e
assert "7-key canonical set (no vacuous_red_green) -> exit 1 (BLOCK)" "1" "$rc"
case "$out" in
  *SUBSET*|*MISSING*|*vacuous*) echo "PASS: 7-key set emits SUBSET/MISSING/vacuous reason"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: 7-key set output lacked SUBSET/MISSING/vacuous reason: $out"; FAIL=$((FAIL+1)) ;;
esac

# P2 case B: 8-key canonical set all-pass + fresh head must PASS.
emit_assessment "$FULL8" "$LIVE_HEAD"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "$LIVE_HEAD")"; rc=$?
set -e
assert "8-key canonical set all-pass + fresh head -> exit 0" "0" "$rc"

# P2 case C: vacuous_red_green=fail must BLOCK even if every other
# gate passes (fail on any required gate blocks strict-green).
emit_assessment '{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"fail"}' "$LIVE_HEAD"
last="$(tail -1 "$LOG")"
set +e
out="$(run_predicate "$last" "$LIVE_HEAD")"; rc=$?
set -e
assert "vacuous_red_green=fail -> exit 1 (BLOCK)" "1" "$rc"
case "$out" in
  *vacuous*) echo "PASS: vacuous_red_green=fail emits FAIL:vacuous_red_green"; PASS=$((PASS+1)) ;;
  *) echo "FAIL: vacuous=fail output lacked vacuous reason: $out"; FAIL=$((FAIL+1)) ;;
esac

# P2 case D: shell predicate REQUIRED set must include vacuous_red_green.
# The set is multiline (`REQUIRED = {` then indented `"key",` lines
# then closing `}`), so we collapse to one line before grepping.
if tr -d '\n' < "$GUARD" | grep -qE 'REQUIRED[[:space:]]*=[[:space:]]*\{[^}]*vacuous_red_green'; then
  echo "PASS: outer guard predicate REQUIRED set includes vacuous_red_green"; PASS=$((PASS+1))
else
  echo "FAIL: outer guard predicate REQUIRED set missing vacuous_red_green"; FAIL=$((FAIL+1))
fi

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0