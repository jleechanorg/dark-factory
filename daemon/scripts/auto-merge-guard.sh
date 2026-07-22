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
# jleechan-328 P1 #1 + jleechan-ni1k #1 (exact-head binding, fail-closed on
# missing live head): refuse to honour an assessment whose recorded
# head_sha no longer matches the live PR head. Without this check, the
# timer-driven merge path can reuse an all-green assessment from an OLDER
# head (a push after a green assessment would merge with stale gate
# evidence). Missing assessed head_sha fails closed — freshness
# unprovable, so we block. Missing LIVE head_sha ALSO fails closed
# (jleechan-ni1k / issue #437 P1): the prior `if live_head and ...` guard
# silently skipped the comparison when `gh pr view --jq .headRefOid` failed
# or returned empty (the `|| true` fallback at line ~119 swallows the
# error), letting a stale assessment promote to merge on a gh-network blip.
# Distinct STALE:LIVE_HEAD_MISSING reason so the Healer can group these
# from generic HEAD_MISSING (which is the assessed-side counterpart).
assessed_head = ctx.get("head_sha") or ""
live_head = sys.argv[1] if len(sys.argv) > 1 else ""
if not assessed_head:
    print("STALE:HEAD_MISSING"); sys.exit(1)
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
# jleechan-328 P1 #2 + jleechan-ni1k #2 (fail-closed canonical gate-key
# set): a 1-key `{"ci_green":"pass"}` subset MUST NOT pass — the predicate
# must prove every canonical gate was actually assessed before strict-
# all-green can hold. Canonical set is `daemon/src/verifier.rs::GateName::
# as_str()`, kept in lockstep with `daemon/factory-overlay.sh`
# REQUIRED_KEYS and `tests/scripts/test_auto_merge_guard_gate_vocabulary.
# sh`. Extra keys (code_standards / zfc) are still permitted as optional
# overlays; only the *absence* of a required key blocks the merge.
#
# jleechan-ni1k / issue #437 P2: gate 8 `vacuous_red_green` (added in
# PR #431 / issue #387 r5) was previously MISSING from this set, so a
# 7-key `{"ci_green":..,"skeptic":..}` report lacking the runtime
# vacuous-test detector verdict could still strict-green even when gate 8
# had never been assessed. Adding the key here closes that bypass; the
# companion gate-vocabulary test
# (tests/scripts/test_auto_merge_guard_gate_vocabulary.sh) covers the
# symmetric case in `factory-overlay.sh` REQUIRED_KEYS.
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

gh pr list --repo "$REPO" --state open --json number,headRefName \
  --jq '.[]|select(.headRefName|startswith("factory/"))|"\(.number) \(.headRefName)"' 2>/dev/null |
while read -r num branch; do
  [ -n "$num" ] || continue
  checks="$(gh pr checks "$num" --repo "$REPO" 2>/dev/null)"
  echo "$checks" | grep -qiE "pending|queued|in_progress" && { echo "PR $num: CI pending — skip"; continue; }
  echo "$checks" | grep -qi "fail" && { echo "PR $num: CI FAILED — skip (needs attention)"; continue; }
  # jleechan-328 P1 #1: pass the LIVE PR head SHA so the predicate can
  # refuse stale assessments (the daemon emits `head_sha` in every
  # GATE_ASSESSMENT context; without the comparison a push after a green
  # assessment would merge with stale gate evidence).
  live_head_sha="$(gh pr view "$num" --repo "$REPO" --json headRefOid --jq .headRefOid 2>/dev/null || true)"
  verdict="$(latest_assessment_no_red "$num" "$live_head_sha")" || { echo "PR $num: verifier assessment ${verdict:-missing} — refusing merge (green CI is insufficient)"; continue; }
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
