#!/usr/bin/env bash
# auto-merge-guard.sh — merge-authority policy gate for factory PRs, separate
# from the coder (research: concentrate merge authority in ONE policy engine, not
# the code author). ONE pass over open factory/* PRs; safe to run on a timer.
#
# A PR merges ONLY when ALL hold (green-CI-is-insufficient — /advice research):
#   1. every CI check has concluded and none FAILED (no pending, no fail)
#   2. the LATEST GATE_ASSESSMENT exists AND has NO red gate. A gate may be
#      green or unknown (unknown = infra unavailability, e.g. CodeRabbit/Bugbot
#      quota walls — NOT a failure); a single `red` gate blocks the merge. This
#      is the honest "no-red" merge policy: strict all-7-green is unachievable
#      here because bot gates are perpetually unknown, so requiring literal
#      all_green=true would deadlock the factory. What must NOT happen is
#      merging on "was assessed" alone while a gate is red.
#   3. the per-hour merge budget is not exhausted (cascade blast-radius cap)
#
# bead jleechan-goal-unattended-e2e-2026-07-17-bze8.1 (U6b): the legacy
# no-red merge policy above left four holes that the factory had to close
# on top of it — disposition notes that substituted for missing evidence,
# PR293/PR300-class regressions (merged-head CHANGES_REQUESTED, rate-
# limited reviewer, stale-SHA PASS, status-context-without-review), and
# silent CodeRabbit gating by a CI check rather than a formal APPROVED
# review. The merge authority is now fail-closed at the exact PR head
# SHA: every gate's evidence is SHA-bound to the live PR head, a CodeRabbit
# record is only honored when `source_id` carries `review:APPROVED`,
# Bugbot must report zero error-severity findings, and the github-actions
# Skeptic verdict must bind to the current head. A disposition note
# (operator assertion) is recorded in the audit telemetry but NEVER
# overrides a missing or Red gate. Per-gate telemetry — source actor,
# source URL/check/review ID, observed SHA, timestamp — is emitted on
# every assessment so the audit trail is reconstructable from one line
# in the daemon log.
#
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

latest_assessment_no_red() { # <pr_number> -> exit 0 if latest GATE_ASSESSMENT exists and has NO red/fail gate
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
# jleechan-240 expand: gate values can be a string ("pass"|"warn"|"fail"|"unknown")
# or a structured object {"verdict": "...", "evidence":[...]}; the merge-authority
# guard must block on any fail verdict, not the literal "red" string the original
# 7-gate schema emitted. Legacy "red" is treated as fail (it was the original
# blocking token); "warn" and "unknown" stay non-blocking per the documented
# no-red merge policy (infra walls like CodeRabbit/Bugbot quota should not
# deadlock the factory).
ALIAS = {"pass":"pass","warn":"warn","fail":"fail","unknown":"unknown",
         "green":"pass","red":"fail","yellow":"warn"}
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
    print("no-fail (unknowns defer: " + ",".join(unknowns) + ")")
else:
    print("no-fail (all gates cleared)")
sys.exit(0)'
}

gh pr list --repo "$REPO" --state open --json number,headRefName \
  --jq '.[]|select(.headRefName|startswith("factory/"))|"\(.number) \(.headRefName)"' 2>/dev/null |
while read -r num branch; do
  [ -n "$num" ] || continue
  checks="$(gh pr checks "$num" --repo "$REPO" 2>/dev/null)"
  echo "$checks" | grep -qiE "pending|queued|in_progress" && { echo "PR $num: CI pending — skip"; continue; }
  echo "$checks" | grep -qi "fail" && { echo "PR $num: CI FAILED — skip (needs attention)"; continue; }
  verdict="$(latest_assessment_no_red "$num")" || { echo "PR $num: verifier assessment ${verdict:-missing} — refusing merge (green CI is insufficient)"; continue; }
  echo "PR $num: assessment $verdict"

  # bead jleechan-goal-unattended-e2e-2026-07-17-bze8.1 (U6b): fail-closed
  # exact-head 7-green merge authority. Re-verify every gate at the LIVE
  # PR head SHA before merging — never trust a stale GATE_ASSESSMENT
  # line. A disposition note or operator assertion can NEVER bypass a
  # missing gate; the verdict comes from per-gate evidence at the
  # exact head only. Per-gate telemetry is emitted on every assessment
  # so the audit trail is reconstructable.
  head_sha="$(gh pr view "$num" --repo "$REPO" --json headRefOid --jq .headRefOid 2>/dev/null || echo "")"
  [ -n "$head_sha" ] || { echo "PR $num: could not resolve live head SHA — refusing merge"; continue; }
  if ! merge_decision="$(python3 -m runner.merge_authority_cli "$num" "$head_sha" "$REPO" 2>/dev/null)"; then
    echo "PR $num: merge-authority call failed (binary missing or non-zero) — refusing merge (fail-closed)"
    continue
  fi
  auth_verdict="$(printf '%s' "$merge_decision" | python3 -c 'import json,sys
try:
    d=json.loads(sys.stdin.read())
except Exception:
    sys.exit(1)
print(d.get("verdict",""))')" || auth_verdict=""
  [ "$auth_verdict" = "MERGE" ] || { echo "PR $num: merge-authority BLOCK (live-head SHA=$head_sha) — refusing merge (fail-closed)"; printf '%s\n' "$merge_decision"; continue; }
  echo "$merge_decision" | python3 -c 'import json,sys
try:
    d=json.loads(sys.stdin.read())
except Exception:
    sys.exit(0)
gates=d.get("gate_telemetry",{})
for name in ["ci_green","no_conflicts","coderabbit","bugbot","comments_resolved","evidence_review","skeptic"]:
    ev=gates.get(name,{})
    print("gate",name,"status="+str(ev.get("status","?")),
          "actor="+str(ev.get("source_actor","")),
          "sha="+str(ev.get("head_sha","")[:12]),
          "id="+str(ev.get("source_id","")))'

  # mergeable?
  [ "$(gh pr view "$num" --repo "$REPO" --json mergeable --jq .mergeable)" = "MERGEABLE" ] || { echo "PR $num: not MERGEABLE (conflicts) — skip"; continue; }
  echo "PR $num: gates red-free + mergeable — merging"
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
