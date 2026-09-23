#!/usr/bin/env bash
set -euo pipefail

# Daily bounded repair sweep. It discovers recently updated, non-draft PRs in
# jleechanorg, then hands only actionable PRs to AO. AO workers may push fixes;
# they must never merge or force-push.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=session-reuse.sh
source "$SCRIPT_DIR/session-reuse.sh"
# shellcheck source=outcome-accounting.sh
source "$SCRIPT_DIR/outcome-accounting.sh"

WINDOW_HOURS="${PR_GREEN_WINDOW_HOURS:-24}"
MAX_PRS="${PR_GREEN_MAX_PRS:-12}"
DRY_RUN="${PR_GREEN_DRY_RUN:-0}"
LOG_PREFIX="[pr-green-daily]"
AO_PROJECT_ROOT="${PR_GREEN_AO_PROJECT_ROOT:-$HOME/.ao/daily-projects}"
METRICS_DIR="${PR_GREEN_METRICS_DIR:-$HOME/.local/state/jleechanorg-pr-green}"
STATE_DIR="$METRICS_DIR/pr-state"
export AO_CONFIG_PATH="${PR_GREEN_AO_CONFIG_PATH:-$HOME/agent-orchestrator.yaml}"
mkdir -p "$METRICS_DIR"
run_started="$(date +%s)"
analyzed=0
actionable=0
attempted=0
dispatched=0

command -v gh >/dev/null || { echo "$LOG_PREFIX gh is required" >&2; exit 127; }
command -v ao >/dev/null || { echo "$LOG_PREFIX ao is required" >&2; exit 127; }

since="$(date -u -d "-${WINDOW_HOURS} hours" '+%Y-%m-%dT%H:%M:%SZ')"
echo "$LOG_PREFIX scanning jleechanorg PRs updated since $since (max $MAX_PRS)"

since_date="${since:0:10}"
prs="$(gh search prs --owner jleechanorg --state open --updated ">=${since_date}" --limit 100 \
  --json repository,number,title,url,updatedAt,isDraft \
  | jq -r --arg since "$since" '.[] | select((.isDraft|not) and .updatedAt >= $since) | [.repository.name, .number, .title, .url, .updatedAt] | @tsv' \
  | sort -k5r | head -n 100)"

# Persist the complete discovery set for the report/audit. Dispatch caps are
# intentionally separate from discovery: a capped repair pass must not pretend
# that later eligible PRs were not found.
discovery_file="$METRICS_DIR/discovery-${run_started}.tsv"
printf '%s\n' "$prs" > "$discovery_file"
discovered_count="$(awk 'NF {n++} END {print n+0}' "$discovery_file")"
echo "$LOG_PREFIX discovered=$discovered_count eligible_recent_non_draft_prs (dispatch cap=$MAX_PRS)"

if [[ -z "$prs" ]]; then
  echo "$LOG_PREFIX no recently updated open PRs"
  exit 0
fi

dispatched=0
selected=0
reused=0
restored=0
busy_deferred=0
fixed_confirmed=0

# outcomes.jsonl is the stable reporting contract. `verified` is true only
# after a fresh GitHub read proves a new head cleared the original blocker.
record_outcome() {
  local repo="$1" number="$2" url="$3" before="$4" after="$5" classification="$6" action="$7"
  local result verified head_before head_after
  read -r result verified <<<"$(pr_green_outcome_result "$classification")"
  head_before="$(jq -r '.head_sha' <<<"$before")"
  head_after="$(jq -r '.head_sha' <<<"$after")"
  jq -cn --argjson ts "$(date +%s)" --argjson run_ts "$run_started" \
    --arg repo "$repo" --argjson number "$number" --arg url "$url" \
    --arg head_before "$head_before" --arg head_after "$head_after" \
    --argjson blocker_before "$before" --argjson blocker_after "$after" \
    --arg classification "$classification" --arg action "$action" \
    --arg result "$result" --argjson verified "$verified" \
    '{ts:$ts,run_ts:$run_ts,repo:$repo,number:$number,url:$url,head_before:$head_before,head_after:$head_after,blocker_before:$blocker_before,blocker_after:$blocker_after,classification:$classification,session_action:$action,result:$result,verified:$verified,detail:($classification + "; " + $action)}' \
    >> "$METRICS_DIR/outcomes.jsonl"
  [[ "$classification" == "fixed_confirmed" ]] && fixed_confirmed=$((fixed_confirmed + 1))
}

reconcile_pr() {
  local repo="$1" number="$2" url="$3" before="$4" action="$5" after classification
  after="$(pr_green_fetch_live_state "$url" 2>/dev/null || true)"
  [[ -n "$after" ]] || { echo "$LOG_PREFIX unable to re-read $repo#$number after $action" >&2; return 1; }
  classification="$(pr_green_classify_outcome "$before" "$after")"
  record_outcome "$repo" "$number" "$url" "$before" "$after" "$classification" "$action"
  echo "$LOG_PREFIX outcome $repo#$number $classification ($action)"
}
while IFS=$'\t' read -r repo number title url updated; do
  [[ -n "$repo" && -n "$number" ]] || continue
  : "$updated" # retained from discovery for the audit TSV ordering
  analyzed=$((analyzed + 1))
  live_state="$(pr_green_fetch_live_state "$url" 2>/dev/null || true)"
  [[ -n "$live_state" ]] || { echo "$LOG_PREFIX unable to inspect $repo#$number" >&2; continue; }
  mergeable="$(jq -r 'if .conflicting then "CONFLICTING" else "MERGEABLE" end' <<<"$live_state")"
  failures="$(jq -r '.failed_checks | join(",")' <<<"$live_state")"
  previous_snapshot="$(pr_green_read_snapshot "$STATE_DIR" "$repo" "$number" || true)"
  if [[ -n "$previous_snapshot" ]]; then
    reconcile_pr "$repo" "$number" "$url" "$previous_snapshot" reconciled || true
  fi
  if [[ "$mergeable" != "CONFLICTING" && -z "$failures" ]]; then
    echo "$LOG_PREFIX skip $repo#$number (no conflict or failed check)"
    continue
  fi
  actionable=$((actionable + 1))
  if (( selected >= MAX_PRS )); then
    echo "$LOG_PREFIX cap reached ($MAX_PRS); deferring $repo#$number to the next run"
    continue
  fi
  selected=$((selected + 1))

  prompt="Work on ${url} in ${repo}. This is an automated daily repair pass for a PR updated in the last ${WINDOW_HOURS} hours. Inspect the exact current PR head and base first. Fix only easy, clearly scoped test failures or mechanical merge conflicts that you can verify locally. Preserve product intent; do not broaden scope, rewrite history, force-push, merge the PR, or change credentials. Run the narrowest relevant tests, then the repository's required checks, commit with an explicit message, and push normally if and only if the fix is green. If the issue is ambiguous, risky, or not mechanically solvable, leave it untouched and report the blocker. Current signals: mergeable=${mergeable:-unknown}; failing_checks=${failures:-none}."

  if [[ "$DRY_RUN" == "1" ]]; then
    printf '%s would dispatch %s#%s (%s)\n' "$LOG_PREFIX" "$repo" "$number" "$title"
    continue
  fi

  # Save the exact blocker/head snapshot immediately before contacting AO.
  pr_green_write_snapshot "$STATE_DIR" "$repo" "$number" "$live_state"

  # Reuse an already configured AO project. For a new repo, clone it into a
  # dedicated non-repository directory before registering it; never clone into
  # the scheduler's working directory.
  project_id="$repo"
  [[ "$repo" == "worldarchitect.ai" ]] && project_id="worldarchitect.ai"
  session_name="pr-${number}"
  session_action=""
  if session_action="$(pr_green_reuse_session "$project_id" "$number" "$prompt")"; then
    case "$session_action" in
      restored)
      attempted=$((attempted + 1))
      restored=$((restored + 1))
      echo "$LOG_PREFIX restored and reused AO session for $repo#$number"
        ;;
      busy_deferred)
      busy_deferred=$((busy_deferred + 1))
      echo "$LOG_PREFIX AO session for $repo#$number is visibly busy; deferred without queuing a prompt"
        ;;
      *)
        attempted=$((attempted + 1))
        reused=$((reused + 1))
      echo "$LOG_PREFIX reused AO session for $repo#$number"
      ;;
    esac
    reconcile_pr "$repo" "$number" "$url" "$live_state" "$session_action" || true
    continue
  else
    session_reuse_rc=$?
    if [[ "$session_reuse_rc" -eq 2 ]]; then
      attempted=$((attempted + 1))
      echo "$LOG_PREFIX existing AO session for $repo#$number rejected update; skipping duplicate spawn" >&2
      reconcile_pr "$repo" "$number" "$url" "$live_state" reuse_rejected || true
      continue
    fi
  fi
  # AO serializes spawns per project; mirror that lock so candidates are
  # deferred instead of producing concurrent-spawn refusals.
  ao_spawn_lock="/run/user/${UID}/jleechanorg-pr-green-ao-${project_id}.lock"
  spawn_cmd=(flock -n "$ao_spawn_lock" ao spawn --project "$project_id" --claim-pr "$number" --name "$session_name" --harness codex --prompt "$prompt")
  attempted=$((attempted + 1))
  spawn_err="$(mktemp)"
  # AO keeps its CLI attached to the worker. Detach it instead of killing the
  # worker on timeout; inspect early output only for stale-worktree recovery.
  "${spawn_cmd[@]}" >"$spawn_err" 2>&1 &
  spawn_pid=$!
  sleep "${PR_GREEN_SPAWN_PROBE_SECONDS:-5}"
  if kill -0 "$spawn_pid" 2>/dev/null; then
    echo "$LOG_PREFIX dispatched $repo#$number (AO worker detached)"
    dispatched=$((dispatched + 1))
    rm -f "$spawn_err"
    reconcile_pr "$repo" "$number" "$url" "$live_state" dispatched || true
    continue
  fi
  if [[ -s "$spawn_err" ]]; then
    cat "$spawn_err" >&2
    # `ao spawn` may successfully create/claim a session and then remain
    # attached to the worker until the timeout. Treat durable creation proof as
    # success; do not kill the newly-created worker just because the CLI stayed
    # attached.
    if pr_green_spawn_output_is_success "$spawn_err"; then
      echo "$LOG_PREFIX dispatched $repo#$number (AO session created; CLI wait bounded)"
      dispatched=$((dispatched + 1))
      rm -f "$spawn_err"
      reconcile_pr "$repo" "$number" "$url" "$live_state" dispatched || true
      continue
    fi
    # AO can retain a reservation for a terminated session whose worktree was
    # removed outside AO. Reap only the exact stale session named by AO, then
    # retry so old worktrees do not require operator intervention.
    stale_session="$(sed -nE 's/.*(wa-[0-9]+).*/\1/p' "$spawn_err" | tail -1)"
    if grep -q 'outside AO-managed worktree directories' "$spawn_err" && [[ -n "$stale_session" ]]; then
      echo "$LOG_PREFIX reaping stale AO session $stale_session and retrying $repo#$number" >&2
      ao session kill "$stale_session" --keep-session >/dev/null 2>&1 || true
      "${spawn_cmd[@]}" >/dev/null 2>&1 &
      echo "$LOG_PREFIX dispatched $repo#$number after stale-session recovery"
      dispatched=$((dispatched + 1))
      rm -f "$spawn_err"
      reconcile_pr "$repo" "$number" "$url" "$live_state" stale_recovery || true
      continue
    fi
    mkdir -p "$AO_PROJECT_ROOT"
    if ! (cd "$AO_PROJECT_ROOT" && ao start "https://github.com/jleechanorg/${repo}" --no-dashboard --no-orchestrator --no-open >/dev/null 2>&1); then
      echo "$LOG_PREFIX failed to register $repo#$number" >&2
      continue
    fi
    "${spawn_cmd[@]}" >/dev/null 2>&1 &
    echo "$LOG_PREFIX dispatched $repo#$number after AO registration"
    dispatched=$((dispatched + 1))
    rm -f "$spawn_err"
    reconcile_pr "$repo" "$number" "$url" "$live_state" registered_and_dispatched || true
    continue
  fi
  rm -f "$spawn_err"
done <<< "$prs"

jq -n --argjson ts "$run_started" --argjson analyzed "$analyzed" \
  --argjson discovered "$discovered_count" \
  --argjson actionable "$actionable" --argjson selected "$selected" \
  --argjson attempted "$attempted" \
  --argjson dispatched "$dispatched" --argjson reused "$reused" \
  --argjson restored "$restored" --argjson busy_deferred "$busy_deferred" \
  --argjson fixed_confirmed "$fixed_confirmed" \
  '{ts:$ts, discovered:$discovered, analyzed:$analyzed, actionable:$actionable, selected:$selected, attempted:$attempted, dispatched:$dispatched, reused:$reused, restored:$restored, busy_deferred:$busy_deferred, fixed_confirmed:$fixed_confirmed}' \
  >> "$METRICS_DIR/runs.jsonl"
echo "$LOG_PREFIX summary analyzed=$analyzed actionable=$actionable selected=$selected attempted=$attempted dispatched=$dispatched reused=$reused restored=$restored busy_deferred=$busy_deferred fixed_confirmed=$fixed_confirmed"
