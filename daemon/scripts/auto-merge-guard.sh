#!/usr/bin/env bash
# auto-merge-guard.sh — merge-authority policy gate for factory PRs, separate
# from the coder (research: concentrate merge authority in ONE policy engine, not
# the code author). ONE pass over open factory/* PRs; safe to run on a timer.
#
# jleechan-bze8.1 / issue #328: a PR merges ONLY when ALL hold:
#   1. every CI check has concluded and none FAILED (no pending, no fail)
#   2. the LATEST GATE_ASSESSMENT is STRICT all-green — every gate verdict
#      is `pass` (or `warn`); a single `unknown` verdict (CodeRabbit
#      unavailable, Bugbot absent, /er pending, unresolved-thread count
#      GraphQL-failed) blocks the merge UNLESS the operator has recorded
#      an explicit disposition via the `operator_disposition` field on the
#      GATE_ASSESSMENT context (token: `OPERATOR_DISPOSITION:`). The old
#      "no-red" predicate permitted `unknown` verdicts and merged
#      #365/#375/#382 with structural-pending unknowns — that is the bug
#      this script now closes. Strict all-green is the only autonomous
#      path to merge; the operator override is the only authorized bypass.
#   3. the per-hour merge budget is not exhausted (cascade blast-radius cap)
# On merge: close the bead, transition the overlay to READY via the harness.
#
# Usage: daemon/scripts/auto-merge-guard.sh [max_merges_per_hour]   (default 8)
set -uo pipefail
cd "$(git rev-parse --show-toplevel)"
REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo jleechanorg/dark-factory)"
LOG="${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
H="daemon/factory-overlay.sh"
MAX_PER_HOUR="${1:-8}"
RATE_FILE="$HOME/.dark-factory/merge-timestamps"
mkdir -p "$(dirname "$RATE_FILE")"; touch "$RATE_FILE"
now_epoch=$(date +%s)

# rate-limit: count merges in the last 3600s
recent=$(awk -v c="$now_epoch" '($1 > c-3600)' "$RATE_FILE" | wc -l | tr -d ' ')
if [ "$recent" -ge "$MAX_PER_HOUR" ]; then
  echo "auto-merge-guard: rate limit ($recent/$MAX_PER_HOUR in last hour) — skipping this pass" >&2
  exit 0
fi

# jleechan-bze8.1 / issue #328: the autonomous merge path must require STRICT
# all-green (every gate verdict green, no unknowns) OR an operator disposition
# record. The unknown-only-after-cap path (#365 / #375 / #382) must produce
# ESCALATION_REQUIRED + HOLD, never READY_FOR_MERGE directly. The earlier
# "no-red" predicate permitted `unknown` verdicts — that is exactly the bug
# the new rule closes. Operator disposition is read from the bead overlay's
# `park_reason` containing the literal token `OPERATOR_DISPOSITION:` plus
# the bead's external_ref in the bead_overlay's `external_disposition` field;
# in practice this means a human must explicitly write
# `OPERATOR_DISPOSITION: <reason>` into the bead note. The presence of any
# such token in the same `GATE_ASSESSMENT` JSONL line's `disposition` field
# is the only authorized bypass.
latest_assessment_strict_all_green() { # <pr_number> -> exit 0 if latest GATE_ASSESSMENT is strict-all-green OR has an operator disposition record
  local pr="$1" last
  last="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$pr[,}]" | tail -1)"
  [ -n "$last" ] || return 1                       # never assessed → block
  printf '%s' "$last" | python3 -c '
import json, sys
try:
    ctx = json.loads(sys.stdin.read())["context"]
    g = ctx["gates"]
except Exception:
    sys.exit(1)                                    # unparseable → block
ALIAS = {"pass":"pass","warn":"pass","fail":"fail","unknown":"unknown",
         "green":"pass","red":"fail","yellow":"pass"}
def verdict(v):
    if isinstance(v, str):
        return ALIAS.get(v, v)
    if isinstance(v, dict):
        return ALIAS.get(v.get("verdict",""), v.get("verdict",""))
    return v
fails = [k for k,v in g.items() if verdict(v) == "fail"]
if fails:
    print("FAIL:" + ",".join(fails)); sys.exit(1)   # any fail → block
unknowns = [k for k,v in g.items() if verdict(v) == "unknown"]
if unknowns:
    # bze8.1: the autonomous merge path requires STRICT all-green (no
    # unknowns) OR an operator disposition record. Without a recorded
    # disposition, the merge must NOT proceed — exit 1 so the calling
    # shell sees an ESCALATION_REQUIRED-style refusal and the bead
    # stays held. The operator override is opt-in and intentional; this
    # is the fix for the #365/#375/#382 regression where unknowns were
    # treated as silently-deferrable and the PR merged anyway.
    disp = ctx.get("operator_disposition", "")
    if disp and "OPERATOR_DISPOSITION:" in str(disp):
        print("unknowns-overridden-by-operator (" + ",".join(unknowns) + ")")
        sys.exit(0)
    print("ESCALATION_REQUIRED (unknowns defer: " + ",".join(unknowns) + ")")
    sys.exit(1)
print("strict-all-green (all gates cleared)")
sys.exit(0)'
}

gh pr list --repo "$REPO" --state open --json number,headRefName \
  --jq '.[]|select(.headRefName|startswith("factory/"))|"\(.number) \(.headRefName)"' 2>/dev/null |
while read -r num branch; do
  [ -n "$num" ] || continue
  checks="$(gh pr checks "$num" --repo "$REPO" 2>/dev/null)"
  echo "$checks" | grep -qiE "pending|queued|in_progress" && { echo "PR $num: CI pending — skip"; continue; }
  echo "$checks" | grep -qi "fail" && { echo "PR $num: CI FAILED — skip (needs attention)"; continue; }
  verdict="$(latest_assessment_strict_all_green "$num")" || { echo "PR $num: verifier assessment ${verdict:-missing} — refusing merge (strict all-green required, or operator disposition; see #328 / bze8.1)"; continue; }
  echo "PR $num: assessment $verdict"
  # mergeable?
  [ "$(gh pr view "$num" --repo "$REPO" --json mergeable --jq .mergeable)" = "MERGEABLE" ] || { echo "PR $num: not MERGEABLE (conflicts) — skip"; continue; }
  echo "PR $num: gates strict-all-green + mergeable — merging"
  gh pr merge "$num" --repo "$REPO" --squash 2>&1 | tail -1
  sleep 3
  if [ "$(gh pr view "$num" --repo "$REPO" --json state --jq .state)" = "MERGED" ]; then
    echo "$now_epoch" >> "$RATE_FILE"
    bead="$(printf '%s' "$branch" | sed -E 's|^factory/||; s|-r[0-9]+$||')"
    br close "$bead" --reason "Merged via factory PR #$num (auto-merge-guard: no-red gate policy verified)" 2>/dev/null | tail -1
    "$H" ready "$bead" "$num" 2>/dev/null | tail -1 || true
    echo "PR $num MERGED, bead $bead closed+READY"
  fi
done
