#!/usr/bin/env bash
# ready-scheduler.sh — READY scheduler for factory PRs.
#
# NOT a merger. This script exists to close Blocker #7 from
# docs/factory-goal-gap-review-2026-07-06.md without taking the merge
# side effect, because the 7-green pre-merge checks are NOT yet enforceable
# (gate 6 /er has no automated runner — bead jleechan-qqq still open).
#
# Per the operator constraint on jleechan-s3c: "treat merge/READY as blocked
# on policy unless the branch only schedules READY without merging ... make
# it a READY scheduler with explicit gate evidence and no merge side effect."
#
# What this DOES, per pass over open factory/* PRs (safe to run on a timer):
#   1. Confirms every CI check has concluded and none FAILED.
#   2. Reads the LATEST GATE_ASSESSMENT for the PR and records the gate
#      evidence (red count, unknown count, green count, missing count) on
#      the READY transition. The gate-evidence JSON IS the audit trail —
#      it is written both as the `context` of the READY_FOR_MERGE event in
#      daemon.jsonl and as a sidecar at daemon/.ready-evidence/<pr>.json so
#      a future, enforce-the-7-gates step can re-derive verdicts.
#   3. If both checks pass AND every required gate is accounted for (green
#      OR unknown — unknown is the honest signal for an infra quota-wall
#      and is not a failure), transitions the bead's overlay to READY via
#      factory-overlay.sh:ready.
#
# What this does NOT do (intentional, per operator constraint):
#   * No `gh pr merge` call.
#   * No `br close` call.
#   * No mutation of the PR head branch or remote.
#   * No rate-limit budget that could be exhausted by future retries.
# The READY state means "this PR has earned the right to be merged by an
# authority that has the full 7-green evidence", which today is the human
# operator. When the 7-green gate becomes enforceable (jleechan-qqq), a
# separate `auto-merge-guard.sh` may be re-enabled under the same
# AUTO_MERGE_DISABLED opt-out cutover pattern.
#
# Usage: daemon/scripts/ready-scheduler.sh
set -uo pipefail
cd "$(git rev-parse --show-toplevel)"
REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo jleechanorg/dark-factory)"
LOG="${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
H="daemon/factory-overlay.sh"

# Pull the latest GATE_ASSESSMENT for a PR and emit a structured record on
# stdout. Exit 0 if found and parsed, 1 otherwise.
latest_assessment_json() { # <pr_number>
  local pr="$1" last
  last="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$pr[,}]" | tail -1)"
  [ -n "$last" ] || { echo '{"found":false,"reason":"no-assessment"}'; return 1; }
  printf '%s' "$last" | python3 -c '
import json, sys
try:
    rec = json.loads(sys.stdin.read())
    ctx = rec.get("context") or {}
    g = ctx.get("gates") or {}
except Exception:
    print(json.dumps({"found": False, "reason": "unparseable"}))
    sys.exit(1)
reds     = sorted(k for k, v in g.items() if v == "red")
unknowns = sorted(k for k, v in g.items() if v == "unknown")
greens   = sorted(k for k, v in g.items() if v == "green")
print(json.dumps({
    "found": True,
    "pr": ctx.get("pr_number"),
    "ts": rec.get("timestamp"),
    "gates": g,
    "red": reds,
    "unknown": unknowns,
    "green": greens,
}))'
}

# Evaluate gate evidence against the 7-green pre-merge inventory.
# stdin: assessment JSON; stdout: lines RED=... UNKNOWN=... GREEN=... MISSING=... TS=...
# Exit 0 if no red AND no missing required gates; exit 1 otherwise.
evaluate_evidence() {
  python3 -c '
import json, sys
a = json.loads(sys.stdin.read())
if not a.get("found"):
    print("REASON:" + a.get("reason", "unknown"))
    sys.exit(2)
required = ["ci","no_conflicts","coderabbit_approved","bugbot_clean",
            "comments_resolved","evidence_floor","skeptic"]
red = a.get("red") or []
unknown = a.get("unknown") or []
green = a.get("green") or []
gates = a.get("gates") or {}
missing = [g for g in required if g not in gates]
print("RED=" + ",".join(red))
print("UNKNOWN=" + ",".join(unknown))
print("GREEN=" + ",".join(green))
print("MISSING=" + ",".join(missing))
print("TS=" + str(a.get("ts") or ""))
sys.exit(0 if not red and not missing else 1)
'
}

EVDIR="$(git rev-parse --show-toplevel)/daemon/.ready-evidence"
mkdir -p "$EVDIR"

gh pr list --repo "$REPO" --state open --json number,headRefName \
  --jq '.[]|select(.headRefName|startswith("factory/"))|"\(.number) \(.headRefName)"' 2>/dev/null |
while read -r num branch; do
  [ -n "$num" ] || continue
  bead="$(printf '%s' "$branch" | sed -E 's|^factory/||; s|-r[0-9]+$||')"

  checks="$(gh pr checks "$num" --repo "$REPO" 2>/dev/null)"
  echo "$checks" | grep -qiE "pending|queued|in_progress" && { echo "PR $num: CI pending — skip"; continue; }
  echo "$checks" | grep -qi "fail" && { echo "PR $num: CI FAILED — skip"; continue; }

  assessment="$(latest_assessment_json "$num")" || { echo "PR $num: no GATE_ASSESSMENT — skip (verifier tier has not assessed)"; continue; }

  eval_out="$(printf '%s' "$assessment" | evaluate_evidence 2>/dev/null)" || true
  rc=$?
  if [ "$rc" -ne 0 ]; then
    reason="refused"
    printf '%s\n' "$eval_out" | grep -q '^REASON:' && reason="$(printf '%s\n' "$eval_out" | sed -n 's/^REASON://p')"
    red="$(printf '%s\n' "$eval_out" | sed -n 's/^RED=//p')"
    missing="$(printf '%s\n' "$eval_out" | sed -n 's/^MISSING=//p')"
    [ -n "$red" ] && reason="$reason red-gate:$red"
    [ -n "$missing" ] && reason="$reason missing-gate:$missing"
    echo "PR $num: READY refused ($reason) — verifier evidence insufficient"
    continue
  fi

  echo "PR $num: gate evidence complete (no reds, all 7 required gates accounted for) — transitioning bead $bead to READY"
  AFD_LOG="$LOG" "$H" ready "$bead" "$num" 2>&1 | tail -1 || echo "PR $num: overlay refused READY (state guard)"
  # Stamp the gate-evidence JSON onto a sidecar file so a future /er-aware
  # merge authority can audit READY transitions without re-deriving them
  # from daemon.jsonl. (No PR mutation; sidecar lives in daemon/.ready-evidence/.)
  printf '%s' "$assessment" > "$EVDIR/$num.json"
done