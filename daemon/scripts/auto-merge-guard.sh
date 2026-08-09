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

# --- API quota preflight (bead rev-1uno): gh pr list/view/checks all call
# GitHub's GraphQL API internally. A GraphQL-only sweep here previously
# drove the shared org graphql quota (user 13840161) to 0/5000, starving
# every other consumer of that quota. Preflight both quotas; route point
# lookups to REST when graphql is low; back off the whole pass (attempt
# zero merges) when both quotas are critically low.
GRAPHQL_LOW="${AMG_GRAPHQL_LOW:-500}"
CORE_LOW="${AMG_CORE_LOW:-200}"
_rl_json="$(gh api rate_limit 2>/dev/null)"
if [ -z "$_rl_json" ]; then
  echo "auto-merge-guard: rate_limit preflight failed (gh api unreachable) — backing off this pass, no merges attempted" >&2
  exit 0
fi
CORE_REMAINING="$(RL_JSON="$_rl_json" RL_KEY=core python3 - 2>/dev/null <<'PYEOF'
import json, os
try:
    d = json.loads(os.environ["RL_JSON"])
    print(d["resources"][os.environ["RL_KEY"]]["remaining"])
except Exception:
    print(0)
PYEOF
)"
GRAPHQL_REMAINING="$(RL_JSON="$_rl_json" RL_KEY=graphql python3 - 2>/dev/null <<'PYEOF'
import json, os
try:
    d = json.loads(os.environ["RL_JSON"])
    print(d["resources"][os.environ["RL_KEY"]]["remaining"])
except Exception:
    print(0)
PYEOF
)"
CORE_REMAINING="${CORE_REMAINING:-0}"
GRAPHQL_REMAINING="${GRAPHQL_REMAINING:-0}"
if [ "$CORE_REMAINING" -lt "$CORE_LOW" ] && [ "$GRAPHQL_REMAINING" -lt "$GRAPHQL_LOW" ]; then
  echo "auto-merge-guard: both quotas low (core=$CORE_REMAINING graphql=$GRAPHQL_REMAINING) — backing off this pass, no merges attempted" >&2
  exit 0
fi
USE_GRAPHQL=1
if [ "$GRAPHQL_REMAINING" -lt "$GRAPHQL_LOW" ]; then
  USE_GRAPHQL=0
  echo "auto-merge-guard: graphql quota low (graphql=$GRAPHQL_REMAINING) — routing point lookups to REST this pass" >&2
fi

RATE_FILE="$HOME/.dark-factory/merge-timestamps"
mkdir -p "$(dirname "$RATE_FILE")"; touch "$RATE_FILE"
now_epoch=$(date +%s)

# rate-limit: count merges in the last 3600s
recent=$(awk -v c="$now_epoch" '($1 > c-3600)' "$RATE_FILE" | wc -l | tr -d ' ')
if [ "$recent" -ge "$MAX_PER_HOUR" ]; then
  echo "auto-merge-guard: rate limit ($recent/$MAX_PER_HOUR in last hour) — skipping this pass" >&2
  exit 0
fi

latest_assessment_no_red() { # <pr_number> <live_head_sha> -> exit 0 if latest GATE_ASSESSMENT exists, matches the live PR head, and has NO red/fail gate
  local pr="$1" live_head="$2" last
  last="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$pr[,}]" | tail -1)"
  [ -n "$last" ] || return 1                       # never assessed → block
  printf '%s' "$last" | python3 -c '
import json, sys
try:
    ctx = json.loads(sys.stdin.read())["context"]
    g = ctx["gates"]
except Exception:
    sys.exit(1)                                    # unparseable → block
# jleechan-328 P1 #1 (exact-head binding): refuse to honour an assessment
# whose recorded head_sha no longer matches the live PR head. Without
# this check, the timer-driven merge path can reuse an all-green
# assessment from an OLDER head (a push after a green assessment would
# merge with stale gate evidence). Missing head_sha fails closed —
# freshness is unprovable, so we block.
assessed_head = ctx.get("head_sha") or ""
live_head = sys.argv[1] if len(sys.argv) > 1 else ""
if not assessed_head:
    print("STALE:HEAD_MISSING"); sys.exit(1)
# jleechan-ni1k / issue #437 P1: refuse silently when `live_head` is
# empty/falsy (the outer guard STALE:LIVE_HEAD_MISSING short-circuits
# earlier on this same condition, but a caller exercising the predicate
# directly with an empty argv[1] would previously fall through to the
# no-fail path because `if live_head and assessed_head != live_head`
# skips the comparison entirely when live_head is falsy). Fail-closed:
# empty live_head means we cannot prove freshness, so block.
if not live_head:
    print("STALE:LIVE_HEAD_MISSING"); sys.exit(1)
if assessed_head != live_head:
    print("STALE:HEAD_MISMATCH:" + assessed_head[:12] + "->" + live_head[:12])
    sys.exit(1)
# jleechan-328 P1 #3 (operator disposition round-trip): single canonical
# field emitted by tick.rs from overlay.park_reason. Surfaced here so the
# shell override can read it from the same key the daemon emits; missing
# → fall through to the standard no-red path (NOT a special token).
operator_disposition = ctx.get("operator_disposition") or ""
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
# jleechan-328 P1 #2 (fail-closed canonical gate-key set): a 1-key
# `{"ci_green":"pass"}` subset MUST NOT pass — the predicate must prove
# every canonical gate was actually assessed before strict-all-green can
# hold. Canonical set is `daemon/src/verifier.rs::GateName::as_str()`,
# kept in lockstep with `daemon/factory-overlay.sh` REQUIRED_KEYS and
# `tests/scripts/test_auto_merge_guard_gate_vocabulary.sh`. Extra keys
# (code_standards / zfc) are still permitted as optional overlays; only
# the *absence* of a required key blocks the merge.
REQUIRED = {"ci_green","no_conflicts","coderabbit","bugbot",
            "comments_resolved","evidence_review","skeptic",
            "vacuous_red_green"}
present = set(g.keys())
missing = sorted(REQUIRED - present)
if missing:
    print("FAIL:SUBSET_MISSING:" + ",".join(missing)); sys.exit(1)
fails = [k for k,v in g.items() if verdict(v) == "fail"]
if fails:
    print("FAIL:" + ",".join(fails)); sys.exit(1)   # any fail → block
unknowns = [k for k,v in g.items() if verdict(v) == "unknown"]
suffix = ""
if operator_disposition:
    suffix = " operator_disposition=" + operator_disposition
if unknowns:
    print("no-fail (unknowns defer: " + ",".join(unknowns) + ")" + suffix)
else:
    print("no-fail (all gates cleared)" + suffix)
sys.exit(0)' "$live_head"
}

if [ "$USE_GRAPHQL" -eq 1 ]; then
  _pr_rows="$(gh pr list --repo "$REPO" --state open --json number,headRefName \
    --jq '.[]|select(.headRefName|startswith("factory/"))|"\(.number) \(.headRefName)"' 2>/dev/null)"
else
  _pr_rows="$(gh api "repos/$REPO/pulls?state=open&per_page=100" \
    --jq '.[]|select(.head.ref|startswith("factory/"))|"\(.number) \(.head.ref)"' 2>/dev/null)"
fi
printf '%s\n' "$_pr_rows" |
while read -r num branch; do
  [ -n "$num" ] || continue
  if [ "$USE_GRAPHQL" -eq 1 ]; then
    checks="$(gh pr checks "$num" --repo "$REPO" 2>/dev/null)"
  else
    _pr_sha="$(gh api "repos/$REPO/pulls/$num" --jq '.head.sha' 2>/dev/null)"
    if [ -n "$_pr_sha" ]; then
      checks="$(gh api "repos/$REPO/commits/$_pr_sha/check-runs" --jq '.check_runs[]|"\(.name) \(.conclusion // .status)"' 2>/dev/null)"
    else
      checks=""
    fi
  fi
  echo "$checks" | grep -qiE "pending|queued|in_progress" && { echo "PR $num: CI pending — skip"; continue; }
  echo "$checks" | grep -qi "fail" && { echo "PR $num: CI FAILED — skip (needs attention)"; continue; }
  # jleechan-328 P1 #1: pass the LIVE PR head SHA so the predicate can
  # refuse stale assessments (the daemon emits `head_sha` in every
  # GATE_ASSESSMENT context; without the comparison a push after a green
  # assessment would merge with stale gate evidence).
  # jleechan-328 P1 #1: pass the LIVE PR head SHA so the predicate can
  # refuse stale assessments (the daemon emits `head_sha` in every
  # GATE_ASSESSMENT context; without the comparison a push after a green
  # assessment would merge with stale gate evidence).
  #
  # jleechan-ni1k / issue #437 P1 (fail-closed on empty live_head_sha):
  # the legacy `|| true` swallowed a transient `gh pr view` failure /
  # empty headRefOid and let the predicate's P1 #1 exact-head check
  # silently skip — a stale assessment from a pre-blip head could then
  # promote to merge. We now (a) capture the gh exit code separately so
  # we can distinguish "no gh output at all" from "gh output is empty",
  # (b) refuse to continue with `continue` if the live head is missing,
  # emitting the distinct `STALE:LIVE_HEAD_MISSING` reason so operators
  # can grep for it on the next tick.
  if [ "$USE_GRAPHQL" -eq 1 ]; then
    live_head_sha="$(gh pr view "$num" --repo "$REPO" --json headRefOid --jq .headRefOid 2>/dev/null)"
    gh_rc=$?
  else
    live_head_sha="$(gh api "repos/$REPO/pulls/$num" --jq .head.sha 2>/dev/null)"
    gh_rc=$?
  fi
  if [ "$gh_rc" -ne 0 ] || [ -z "${live_head_sha:-}" ] || [ "$live_head_sha" = "null" ]; then
    echo "PR $num: STALE:LIVE_HEAD_MISSING — refusing merge (gh_rc=${gh_rc}, live_head_sha=${live_head_sha:-empty}; gh api outage or transient error must not promote stale gate evidence)"
    continue
  fi
  verdict="$(latest_assessment_no_red "$num" "$live_head_sha")" || { echo "PR $num: verifier assessment ${verdict:-missing} — refusing merge (green CI is insufficient)"; continue; }
  echo "PR $num: assessment $verdict"
  # mergeable?
  if [ "$USE_GRAPHQL" -eq 1 ]; then
    _mergeable="$(gh pr view "$num" --repo "$REPO" --json mergeable --jq .mergeable 2>/dev/null)"
    [ "$_mergeable" = "MERGEABLE" ] || { echo "PR $num: not MERGEABLE (conflicts) — skip"; continue; }
  else
    _mergeable="$(gh api "repos/$REPO/pulls/$num" --jq '.mergeable' 2>/dev/null)"
    [ "$_mergeable" = "true" ] || { echo "PR $num: not MERGEABLE (conflicts) — skip"; continue; }
  fi
  echo "PR $num: gates red-free + mergeable — merging"
  gh pr merge "$num" --repo "$REPO" --squash 2>&1 | tail -1
  sleep 3
  if [ "$USE_GRAPHQL" -eq 1 ]; then
    _merged_state="$(gh pr view "$num" --repo "$REPO" --json state --jq .state 2>/dev/null)"
    _is_merged=false; [ "$_merged_state" = "MERGED" ] && _is_merged=true
  else
    _is_merged="$(gh api "repos/$REPO/pulls/$num" --jq '.merged' 2>/dev/null)"
  fi
  if [ "$_is_merged" = "true" ]; then
    echo "$now_epoch" >> "$RATE_FILE"
    bead="$(printf '%s' "$branch" | sed -E 's|^factory/||; s|-r[0-9]+$||')"
    br close "$bead" --reason "Merged via factory PR #$num (auto-merge-guard: no-red gate policy verified)" 2>/dev/null | tail -1
    "$H" ready "$bead" "$num" 2>/dev/null | tail -1 || true
    echo "PR $num MERGED, bead $bead closed+READY"
  fi
done
