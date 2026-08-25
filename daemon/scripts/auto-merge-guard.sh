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

# --- Repo auto-merge policy gate (2026-08-23 PR-merge-storm incident) ---
# Config-only, not code: which repos this script may auto-merge in is
# controlled ENTIRELY by config/auto_merge_repo_allowlist.json (or the
# AMG_REPO_POLICY_FILE override) -- no repo name is ever hardcoded here.
# Incident: 2026-08-23, a burst of unattended worldai merges (42 PRs in 12h,
# most with no literal human MERGE APPROVED at time of merge) produced real
# production regressions (PATCH /api/campaigns/<id> whitelist stripped,
# context-compression pruning wiped). This script -- the one dark-factory
# component whose literal job is "merge-authority policy gate" -- had zero
# human-approval step or repo scoping for any repo it might run against.
# Operator directive: keep the factory dispatch daemon running, stop only
# the merges, and control the stop/resume purely via config so re-enabling
# a repo never requires a code change or redeploy -- just an edit to the
# allowlist file. Absence of the config file, an empty list, or $REPO not
# present in it all mean "no merges this pass" -- the safe default is off.
AMG_REPO_POLICY_FILE="${AMG_REPO_POLICY_FILE:-$(git rev-parse --show-toplevel 2>/dev/null)/config/auto_merge_repo_allowlist.json}"
if [ ! -f "$AMG_REPO_POLICY_FILE" ]; then
  echo "auto-merge-guard: no repo allowlist config at $AMG_REPO_POLICY_FILE — refusing to merge anything this pass (fail-closed default)" >&2
  exit 0
fi
_repo_allowed="$(python3 -c "
import json, sys
try:
    with open('$AMG_REPO_POLICY_FILE') as f:
        cfg = json.load(f)
    allowed = cfg.get('allowed_repos', [])
    print('true' if '$REPO' in allowed else 'false')
except Exception as e:
    print('false')
" 2>/dev/null)"
if [ "$_repo_allowed" != "true" ]; then
  echo "auto-merge-guard: $REPO is not in the allowed_repos list at $AMG_REPO_POLICY_FILE — refusing to merge anything this pass (fail-closed default)" >&2
  exit 0
fi

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

# --- checks_all_green (codex skeptic review REQUEST_CHANGES on #619/#620/#621
# follow-up): the ORIGINAL checks-evaluation block only grepped raw check-run
# text for "pending|queued|in_progress" (not-yet-green) or "fail" (red). Three
# fail-open gaps that let a non-green PR slip through as "green":
#   1. CANCELLED / TIMED_OUT conclusions contain neither substring — they
#      matched neither grep and the PR was silently treated as green.
#   2. The REST check-runs call had no pagination — `gh api` defaults to 30
#      items/page, so a check-run past item 30 could never be seen.
#   3. An EMPTY check-run list (e.g. transient API hiccup, or a PR with zero
#      registered runs) matched NEITHER grep either — empty-is-not-pending
#      and empty-is-not-fail, so it also fell through as "green with zero
#      evidence". This was the most dangerous gap: no evidence should never
#      read as affirmative evidence.
#   4. The legacy commit-status API (separate from check-runs) was never
#      consulted at all.
# checks_all_green() closes all four: REST path uses `--paginate` and treats
# anything other than status=completed + conclusion in
# {success,neutral,skipped} as NOT green (this structurally also covers
# cancelled/timed_out/action_required/stale — none of those are in the
# allowed set); an empty check-run list is explicitly NOT green; and the
# legacy status API is consulted too (empty/never-used is fine, but a
# present non-success legacy status blocks). The GraphQL path (er-delta
# follow-up on the /advice error-state finding) no longer scrapes the
# `gh pr checks` text table at all: it reads `gh pr checks --json bucket`,
# gh's closed 5-value categorization (pass/fail/pending/skipping/cancel)
# applied to every raw conclusion — only bucket in {pass,skipping} counts
# as green, empty output is NOT green, and anything else (fail, pending,
# cancel — which is where error/timed_out/action_required/startup_failure
# and any future conclusion land) is NOT green. Reading a structured field
# closes the missing-keyword bug class instead of enumerating keywords.
checks_all_green() { # <pr_number> <live_head_sha> -> prints "GREEN:..."/"NOT_GREEN:reason", returns 0/1
  local pr="$1" head="$2"
  if [ "$USE_GRAPHQL" -eq 1 ]; then
    local checks
    # codex /advice follow-up: the previous free-text substring scrape
    # (grep for "cancelled|timed_out|...") omitted "error" despite the
    # header comment claiming it was covered -- a checks table with one
    # line in an error/unknown state and other lines passing slipped
    # through as green (confirmed live: a fixture with 2 "pass" lines and
    # one "error" line returned GREEN:GRAPHQL). Free-text scraping is also
    # inherently unsafe: a CHECK NAME containing a bad-state word (e.g. a
    # check literally named "error-handling-test") could false-positive
    # block, or a not-yet-seen raw conclusion word could false-negative
    # pass. `gh pr checks --json bucket` sidesteps both: gh categorizes
    # every possible state into exactly one of 5 authoritative buckets
    # (pass, fail, pending, skipping, cancel) -- reading that structured
    # field is token-position-exact, not a text scrape, and "fail"
    # structurally absorbs every failure-class raw state (error,
    # timed_out, action_required, startup_failure, ...) without needing to
    # enumerate them.
    local buckets
    buckets="$(gh pr checks "$pr" --repo "$REPO" --json bucket --jq '.[].bucket' 2>/dev/null)"
    if [ -z "$buckets" ]; then
      echo "NOT_GREEN:EMPTY_CHECKS"; return 1
    fi
    local bad_bucket
    bad_bucket="$(printf '%s\n' "$buckets" | grep -vE '^(pass|skipping)$' | head -1)"
    if [ -n "$bad_bucket" ]; then
      echo "NOT_GREEN:BAD_BUCKET:$bad_bucket"; return 1
    fi
    echo "GREEN:GRAPHQL"; return 0
  fi
  # REST path: paginate check-runs (gh api defaults to 30/page — a bad run
  # past item 30 must not be invisible) and require every run to be
  # status=completed with conclusion in {success,neutral,skipped}.
  local runs
  runs="$(gh api "repos/$REPO/commits/$head/check-runs" --paginate \
    --jq '.check_runs[]|"\(.status) \(.conclusion // "null")"' 2>/dev/null)"
  if [ -z "$runs" ]; then
    echo "NOT_GREEN:EMPTY_CHECK_RUNS"; return 1
  fi
  local bad
  bad="$(printf '%s\n' "$runs" | awk '
    { status=$1; conclusion=$2 }
    status != "completed" { print; next }
    conclusion != "success" && conclusion != "neutral" && conclusion != "skipped" { print }
  ')"
  if [ -n "$bad" ]; then
    echo "NOT_GREEN:CHECK_RUNS:$(printf '%s' "$bad" | tr '\n' ';')"; return 1
  fi
  # Legacy combined commit-status API — separate from check-runs. Empty /
  # never-used (total_count=0) is fine (no legacy statuses to fail on); a
  # present non-success state blocks.
  local status_json status_rc status_total status_state
  status_json="$(gh api "repos/$REPO/commits/$head/status" 2>/dev/null)"
  status_rc=$?
  if [ "$status_rc" -ne 0 ]; then
    echo "NOT_GREEN:LEGACY_STATUS_API_ERROR:rc=$status_rc"; return 1
  fi
  if [ -n "$status_json" ]; then
    # Strict parse: total_count MUST be present as an actual key, or this
    # is not a genuine commit-status response -- e.g. a GH API error body
    # like {"message":"...","documentation_url":"..."} has neither
    # "total_count" nor "state". PARSE_ERROR/MISSING both fail closed
    # (skip the PR); only a real, well-formed response with
    # total_count==0 is treated as "no legacy statuses configured" (fine,
    # falls through).
    status_total="$(printf '%s' "$status_json" | python3 -c '
import json, sys
try:
    d = json.loads(sys.stdin.read())
    if "total_count" not in d:
        print("MISSING")
    else:
        print(d["total_count"])
except Exception:
    print("PARSE_ERROR")
' 2>/dev/null)"
    case "$status_total" in
      MISSING|PARSE_ERROR|"")
        echo "NOT_GREEN:LEGACY_STATUS_API_ERROR:unparseable_or_missing_total_count"; return 1
        ;;
      0)
        : # genuinely zero legacy statuses configured -- fine
        ;;
      *)
        status_state="$(printf '%s' "$status_json" | python3 -c '
import json, sys
try:
    d = json.loads(sys.stdin.read())
    print(d.get("state") or "")
except Exception:
    print("")
' 2>/dev/null)"
        if [ "$status_state" != "success" ]; then
          echo "NOT_GREEN:LEGACY_STATUS:${status_state:-unknown}"; return 1
        fi
        ;;
    esac
  fi
  echo "GREEN:REST"; return 0
}

# --- Human-approval marker gate (bead rev-iwywa, 2026-08-23 incident
# follow-up): PR #735 added a fail-closed repo allowlist, but CLAUDE.md's
# own "MERGE APPROVED" policy was, until now, enforced by session-prompt
# convention ONLY — zero code-level check existed anywhere in this script
# or gh-pr-merge-wrapper.sh. The moment a repo is re-added to
# config/auto_merge_repo_allowlist.json (a one-line JSON edit -- that's
# the whole point of PR #735's design), the exact unattended-merge gap
# that caused the incident reopens with zero additional protection,
# because the allowlist alone doesn't prove a human approved THIS PR.
# This is a SECOND, INDEPENDENT gate — it augments, not replaces, the
# repo-allowlist gate above and the no-red gate checks in
# latest_assessment_no_red(). A PR is approved when EITHER:
#   (a) a PR comment containing a standalone line that is exactly
#       "MERGE APPROVED" (case-sensitive, anchored -- not merely a
#       substring anywhere in the body, so a negation like "does NOT say
#       MERGE APPROVED" or a quoted/code-block mention never satisfies
#       this) was posted by a non-bot account that is NOT the PR's own
#       author (an author commenting on their own PR must never satisfy
#       the gate -- that would make the marker trivially self-spoofable), OR
#   (b) the $AMG_APPROVAL_LABEL label ("auto-merge-approved" by default)
#       was applied by a non-bot GitHub actor (checked via the issue
#       events/timeline, not just current label presence, so we know WHO
#       applied it).
# Fail-closed throughout: any lookup failure, empty response, or
# unparseable JSON means NOT approved.
AMG_APPROVAL_LABEL="${AMG_APPROVAL_LABEL:-auto-merge-approved}"
human_approval_marker_present() { # <pr_number> -> exit 0 iff a valid human approval marker is present
  local pr="$1" author_login comments_json events_json
  author_login="$(gh api "repos/$REPO/pulls/$pr" --jq '.user.login // ""' 2>/dev/null)"
  # --slurp wraps each page's array as its own element (so multi-page
  # results parse as valid JSON) -- without it, gh api --paginate prints
  # each page as a SEPARATE, back-to-back JSON array/object for any
  # endpoint with more than one page of results, which is not valid JSON
  # on its own and made json.loads() silently fail (caught by a bare
  # except, comments/events treated as empty) on any PR with a long
  # comment or event history.
  comments_json="$(gh api "repos/$REPO/issues/$pr/comments" --paginate --slurp 2>/dev/null)"
  if [ -n "$comments_json" ]; then
    if PR_AUTHOR="$author_login" python3 -c '
import json, os, re, sys

AUTHOR = os.environ.get("PR_AUTHOR", "")
# Fail-closed: if we could not determine who the PR author is (the
# earlier `gh api .../pulls/$pr` lookup failed or returned an empty
# login), we cannot enforce the author-exclusion below, so refuse via
# the comment path entirely rather than silently allowing the true
# author (now indistinguishable from anyone else) to self-approve.
if not AUTHOR:
    sys.exit(1)

# Anchored, standalone-line match only (mirrors this repo own
# _parse_verdict marker convention, and CLAUDE.md rule that MERGE APPROVED
# must appear verbatim) -- a bare substring search would let any comment
# that merely contains the phrase satisfy the gate, including negations
# ("does NOT say MERGE APPROVED"), quotes, or code-block embeddings.
MARKER = re.compile(r"(?m)^\s*MERGE APPROVED\s*$")
FENCE = re.compile(r"```.*?```", re.DOTALL)

def is_bot(user):
    if not isinstance(user, dict):
        return True
    if (user.get("type") or "") == "Bot":
        return True
    login = user.get("login") or ""
    return login.endswith("[bot]") or login.endswith("-bot")

try:
    pages = json.loads(sys.stdin.read())
except Exception:
    pages = []
if not isinstance(pages, list):
    pages = []
comments = []
for page in pages:
    if isinstance(page, list):
        comments.extend(page)
for c in comments:
    body = c.get("body") or ""
    # Strip fenced code blocks before matching so a reviewer quoting the
    # marker as a documentation/policy example inside a ``` block never
    # satisfies the gate -- only a real, unfenced approval line counts.
    body_unfenced = FENCE.sub("", body)
    if not MARKER.search(body_unfenced):
        continue
    user = c.get("user") or {}
    if is_bot(user):
        continue
    login = user.get("login") or ""
    if login == AUTHOR:
        continue
    print("APPROVED:COMMENT:" + login)
    sys.exit(0)
sys.exit(1)
' <<<"$comments_json"; then
      return 0
    fi
  fi
  events_json="$(gh api "repos/$REPO/issues/$pr/events" --paginate --slurp 2>/dev/null)"
  if [ -n "$events_json" ]; then
    if LABEL="$AMG_APPROVAL_LABEL" PR_AUTHOR="$author_login" python3 -c '
import json, os, sys

LABEL = os.environ.get("LABEL", "")
AUTHOR = os.environ.get("PR_AUTHOR", "")
# Fail-closed for the same reason as the comment path above: if we could
# not determine the PR author, we cannot enforce author-exclusion, so
# refuse via the label path entirely.
if not AUTHOR:
    sys.exit(1)

def is_bot(actor):
    if not isinstance(actor, dict):
        return True
    if (actor.get("type") or "") == "Bot":
        return True
    login = actor.get("login") or ""
    return login.endswith("[bot]") or login.endswith("-bot")

try:
    pages = json.loads(sys.stdin.read())
except Exception:
    pages = []
if not isinstance(pages, list):
    pages = []
events = []
for page in pages:
    if isinstance(page, list):
        events.extend(page)

# Reconstruct CURRENT label state from the chronological event history
# (the /issues/$pr/events endpoint returns events oldest-first) rather
# than approving on the first "labeled" event found: a label that was
# applied and later REMOVED must not still count as approval, and the
# actor recorded must be whoever most recently (re-)applied it, not
# whoever happened to apply it first.
current_actor = None
for e in events:
    if e.get("event") not in ("labeled", "unlabeled"):
        continue
    label_name = (e.get("label") or {}).get("name") or ""
    if label_name != LABEL:
        continue
    if e.get("event") == "unlabeled":
        current_actor = None
        continue
    actor = e.get("actor") or {}
    if is_bot(actor):
        current_actor = None
        continue
    login = actor.get("login") or ""
    if login == AUTHOR:
        current_actor = None
        continue
    current_actor = login

if current_actor:
    print("APPROVED:LABEL:" + current_actor)
    sys.exit(0)
sys.exit(1)
' <<<"$events_json"; then
      return 0
    fi
  fi
  return 1
}

if [ "$USE_GRAPHQL" -eq 1 ]; then
  _pr_rows="$(gh pr list --repo "$REPO" --state open --json number,headRefName \
    --jq '.[]|select(.headRefName|startswith("factory/"))|"\(.number) \(.headRefName)"' 2>/dev/null)"
else
  _pr_rows="$(gh api "repos/$REPO/pulls?state=open&per_page=100" \
    --jq '.[]|select(.head.ref|startswith("factory/"))|"\(.number) \(.head.ref)"' 2>/dev/null)"
fi

# --- bead rev-wngvy: bound the REST sweep by remaining CORE quota. When
# routing point lookups to REST (USE_GRAPHQL=0), each open factory/* PR
# costs several REST calls (live head SHA, paginated check-runs, legacy
# status, mergeable, merge, post-merge state) — a sweep over many open PRs
# can itself exhaust the core quota it was supposed to conserve. Estimate
# the sweep's own cost from the PR count just fetched and back off (same
# fail-safe pattern as the GraphQL backoff above) if core can't cover it.
if [ "$USE_GRAPHQL" -eq 0 ]; then
  _rest_pr_count="$(printf '%s\n' "$_pr_rows" | grep -c . || true)"
  AMG_REST_CALLS_PER_PR="${AMG_REST_CALLS_PER_PR:-6}"
  AMG_REST_SAFETY_MARGIN="${AMG_REST_SAFETY_MARGIN:-50}"
  _rest_cost_estimate=$((_rest_pr_count * AMG_REST_CALLS_PER_PR + AMG_REST_SAFETY_MARGIN))
  if [ "$CORE_REMAINING" -lt "$_rest_cost_estimate" ]; then
    echo "auto-merge-guard: core quota (remaining=$CORE_REMAINING) insufficient for REST sweep of $_rest_pr_count open factory PR(s) (estimated cost=$_rest_cost_estimate) — backing off this pass, no merges attempted" >&2
    exit 0
  fi
fi

printf '%s\n' "$_pr_rows" |
while read -r num branch; do
  [ -n "$num" ] || continue
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
  #
  # Consolidated (codex skeptic follow-up): this used to be computed a
  # second time, mid-loop, after the checks grep — duplicating this exact
  # `gh api repos/.../pulls/$num --jq .head.sha` call. Computing it here,
  # first, means checks_all_green() can reuse it directly (no duplicate
  # REST round-trip) AND the STALE:LIVE_HEAD_MISSING fail-closed guard now
  # runs before ANY checks evaluation — fail closed even earlier.
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
  checks_reason="$(checks_all_green "$num" "$live_head_sha")"
  checks_rc=$?
  if [ "$checks_rc" -ne 0 ]; then
    _runner_probe_output="$(scripts/check_runner_health.sh "$REPO" 2>&1)"
    _runner_probe_rc=$?
    if [ "$_runner_probe_rc" -eq 3 ]; then
      _runner_warning="RUNNER FLEET DOWN — wait for org runner recovery; merge policy remains enforced"
      echo "PR $num: $_runner_warning"
      if ! gh pr view "$num" --repo "$REPO" --comments --json comments --jq '.comments[].body' 2>/dev/null | grep -qF "$_runner_warning"; then
        gh pr comment "$num" --repo "$REPO" --body "$_runner_warning" 2>/dev/null || true
      fi
    elif [ "$_runner_probe_rc" -eq 1 ]; then
      echo "PR $num: RUNNER SELECTOR DRIFT — configured labels match no online org runner"
    elif [ "$_runner_probe_rc" -ne 0 ]; then
      echo "PR $num: RUNNER STATUS INCONCLUSIVE — not classifying as an outage"
    fi
    echo "PR $num: CI not green ($checks_reason) — skip"; continue
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
  # bead rev-iwywa: SECOND, independent gate -- every other gate above can
  # pass (CI green, assessment green, mergeable, repo-allowlisted) and this
  # PR must still be refused without a human-posted approval marker. See
  # human_approval_marker_present() above for the full anti-spoofing
  # rationale (non-author, non-bot).
  approval_reason="$(human_approval_marker_present "$num")"
  approval_rc=$?
  if [ "$approval_rc" -ne 0 ]; then
    echo "PR $num: REFUSED:NO_APPROVAL_MARKER — no human-posted 'MERGE APPROVED' comment or '$AMG_APPROVAL_LABEL' label (non-author, non-bot) found — refusing merge"
    continue
  fi
  echo "PR $num: approval marker ($approval_reason)"
  echo "PR $num: gates red-free + mergeable — merging"
  # bead rev-377j4: attribute the merge-provenance JSONL line (written by
  # gh-pr-merge-wrapper.sh, the sole caller of `gh pr merge`) to this
  # automated timer path rather than the "manual-wrapper-invocation"
  # default a direct/manual wrapper call would get.
  AMG_TRIGGERING_MECHANISM="auto-merge-guard.sh" GH_REPO="$REPO" "$(dirname "${BASH_SOURCE[0]}")/gh-pr-merge-wrapper.sh" "$num" --squash 2>&1 | tail -2
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
