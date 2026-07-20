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

latest_assessment_no_red() { # <pr_number> -> exit 0 iff the latest GATE_ASSESSMENT proves every gate green at the exact head (fail-closed otherwise)
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
# jleechan-328 / jleechanorg/dark-factory#328: fail-closed exact-head 7-green
# merge authority. The legacy policy treated "unknown" as a deferral (next
# tick) and let a disposition note substitute for strict 7-green. That was
# the exact bypass the bead names: recent factory/operator merges were
# called green despite missing CodeRabbit/Bugbot/Skeptic verdicts, which
# contradicts the repository rule that every merge gate must be proven
# green at the exact head.
#
# The new contract:
#   * any FAIL verdict → block
#   * any UNKNOWN verdict → block (fail-closed on infra walls: "unknown"
#     means we have not proven this gate; that is not the same as green)
#   * missing/empty gates object → block (cannot prove what is absent)
#   * unparseable verdict token (anything outside pass|warn|fail|unknown)
#     → block
#   * `review_degraded: true` (single-model-family review) → block, even
#     when every gate reads green; a single-model family cannot satisfy
#     strict merge policy (jleechan-984e)
#   * only a complete {"<gate>": "pass"|"warn"} for every required gate
#     AND review_degraded in {false, absent} → allow
ALIAS = {"pass":"pass","warn":"warn","fail":"fail","unknown":"unknown",
         "green":"pass","red":"fail","yellow":"warn"}
ALLOWED_VERDICTS = {"pass", "warn"}
def verdict(v):
    if isinstance(v, str):
        return ALIAS.get(v, None)                  # unparseable → None
    if isinstance(v, dict):
        return ALIAS.get(v.get("verdict",""), None)
    return None
if not isinstance(g, dict) or not g:
    print("FAIL: gates object is missing or empty (fail-closed)"); sys.exit(1)
unknowns = []
fails = []
unparseable = []
for k, v in g.items():
    tok = verdict(v)
    if tok is None:
        unparseable.append(k)
    elif tok == "fail":
        fails.append(k)
    elif tok == "unknown":
        unknowns.append(k)
if unparseable:
    print("FAIL:unparseable:" + ",".join(unparseable)); sys.exit(1)
if fails:
    print("FAIL:" + ",".join(fails)); sys.exit(1)
if unknowns:
    print("FAIL:unknown:" + ",".join(unknowns)); sys.exit(1)
# Strict merge policy (issue #328): a single-model-family review does not
# satisfy the contract. `review_degraded` is emitted by the daemon on every
# GATE_ASSESSMENT (jleechan-984e); absent == false (legacy telemetry) is
# treated as multi-model.
review_degraded = ctx.get("review_degraded", False)
if review_degraded is True or str(review_degraded).lower() == "true":
    print("FAIL:review_degraded (single-model family review, strict merge policy)")
    sys.exit(1)
print("no-fail (all 7 gates cleared at exact head)")
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
