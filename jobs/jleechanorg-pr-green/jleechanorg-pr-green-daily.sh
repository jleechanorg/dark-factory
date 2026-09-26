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
GLOBAL_ACTIVE_CAP="${PR_GREEN_GLOBAL_ACTIVE_CAP:-30}"
BATCH_CAP=15
if ! [[ "$MAX_PRS" =~ ^[0-9]+$ ]]; then MAX_PRS=12; fi
if ! [[ "$GLOBAL_ACTIVE_CAP" =~ ^[0-9]+$ ]]; then GLOBAL_ACTIVE_CAP=30; fi
if (( MAX_PRS > BATCH_CAP )); then MAX_PRS="$BATCH_CAP"; fi
DRY_RUN="${PR_GREEN_DRY_RUN:-0}"
LOG_PREFIX="[pr-green-daily]"
AO_PROJECT_ROOT="${PR_GREEN_AO_PROJECT_ROOT:-$HOME/.ao/daily-projects}"
METRICS_DIR="${PR_GREEN_METRICS_DIR:-$HOME/.local/state/jleechanorg-pr-green}"
STATE_DIR="$METRICS_DIR/pr-state"
export PR_GREEN_DELIVERY_STATE_DIR="${PR_GREEN_DELIVERY_STATE_DIR:-$METRICS_DIR/pending-delivery}"
AO_SPAWN_LOCK_DIR="${PR_GREEN_AO_SPAWN_LOCK_DIR:-/run/user/${UID}}"
GLOBAL_ADMISSION_LOCK_FILE="$AO_SPAWN_LOCK_DIR/jleechanorg-pr-green-global-admission.lock"
PUSH_RECEIPT_HELPER="$SCRIPT_DIR/push-receipt.sh"
export AO_CONFIG_PATH="${PR_GREEN_AO_CONFIG_PATH:-$HOME/agent-orchestrator.yaml}"
mkdir -p "$METRICS_DIR"
mkdir -p "$AO_SPAWN_LOCK_DIR"
exec 8>"$GLOBAL_ADMISSION_LOCK_FILE"
run_started="${PR_GREEN_RUN_STARTED:-$(date +%s)}"
analyzed=0
analysis_failed=0
actionable=0
attempted=0
dispatched=0

# Count all non-terminated AO sessions through the daemon's own SQLite source.
# This is deliberately a read-only aggregate: project-scoped session APIs do
# not expose a safe global count, and an unreadable source must fail closed.
pr_green_global_active_count() {
  local db count
  command -v sqlite3 >/dev/null 2>&1 || return 1
  db="${PR_GREEN_AO_DB_PATH:-$HOME/.ao/data/ao.db}"
  [[ -r "$db" ]] || return 1
  count="$(sqlite3 -noheader "$db" \
    "SELECT COUNT(*) FROM sessions WHERE is_terminated = 0;" 2>/dev/null | head -n 1)"
  [[ "$count" =~ ^[0-9]+$ ]] || return 1
  printf '%s\n' "$count"
}

pr_green_admission_available() {
  local active_count
  active_count="$(pr_green_global_active_count || true)"
  [[ "$active_count" =~ ^[0-9]+$ ]] || {
    echo "$LOG_PREFIX active-session count unavailable; suppressing new admission" >&2
    return 1
  }
  (( active_count < GLOBAL_ACTIVE_CAP ))
}

pr_green_release_admission_lock() {
  flock -u 8 2>/dev/null || true
}

# session-reuse.sh calls this immediately before restoring a terminated row.
# The row is re-read by that helper, so a live->terminated transition cannot
# bypass the same global cap used by new spawns.
pr_green_before_restore_admission() {
  if (( admission_lock_held == 1 )); then
    pr_green_admission_available
    return
  fi
  flock -n 8 || return 1
  admission_lock_held=1
  if ! pr_green_admission_available; then
    pr_green_release_admission_lock
    admission_lock_held=0
    return 1
  fi
}

command -v gh >/dev/null || { echo "$LOG_PREFIX gh is required" >&2; exit 127; }
command -v ao >/dev/null || { echo "$LOG_PREFIX ao is required" >&2; exit 127; }

since="$(date -u -d "-${WINDOW_HOURS} hours" '+%Y-%m-%dT%H:%M:%SZ')"
echo "$LOG_PREFIX scanning jleechanorg PRs updated since $since (max $MAX_PRS)"

since_date="${since:0:10}"
# REST Search is exhaustively paginated only below GitHub's documented
# 1,000-result ceiling. Validate every page before trusting it; an incomplete
# response or a count mismatch falls back to repository-wise PR pagination.
discover_prs() {
  local search_json_file search_total search_items search_complete repos_json_file repo pulls repo_names_file
  search_json_file="$(mktemp "${TMPDIR:-/tmp}/pr-green-discovery.XXXXXX")"
  repos_json_file=""
  if ! gh api --paginate --slurp -X GET /search/issues \
    -f "q=org:jleechanorg is:pr is:open updated:>=${since_date}" -f per_page=100 \
    >"$search_json_file"; then
    rm -f -- "$search_json_file"
    return 1
  fi
  search_total="$(jq -r '([.[].total_count? // 0] | max) // 0' "$search_json_file")" || {
    rm -f -- "$search_json_file"
    return 1
  }
  search_items="$(jq -r '[.[].items[]?] | length' "$search_json_file")" || {
    rm -f -- "$search_json_file"
    return 1
  }
  search_complete="$(jq -r '
    ([.[].incomplete_results? // false] | any) as $incomplete
    | ([.[].total_count?] | all(type == "number")) as $counts_numeric
    | ($counts_numeric and ($incomplete | not))
  ' "$search_json_file" 2>/dev/null || true)"
  if ! jq -n --arg cutoff "$since" --arg source search \
    --argjson total "$search_total" \
    --argjson item_count "$search_items" --argjson complete "$search_complete" \
    --slurpfile pages "$search_json_file" \
    '{cutoff:$cutoff,source:$source,pages:$pages[0],total_count:$total,item_count:$item_count,incomplete_results:($complete|not)}' \
    >"$METRICS_DIR/discovery-${run_started}.json"; then
    rm -f -- "$search_json_file"
    return 1
  fi
  if [[ "$search_complete" == true ]] && (( search_total <= 1000 )) && (( search_items == search_total )); then
    if ! jq -r --arg since "$since" '
      .[] | .items[]?
      | select(.updated_at >= $since and ((.draft // false) | not))
      | [(.repository_url | split("/") | .[-1]), .number, .title, .html_url, .updated_at]
      | @tsv
    ' "$search_json_file"; then
      rm -f -- "$search_json_file"
      return 1
    fi
    rm -f -- "$search_json_file"
    return 0
  fi

  echo "$LOG_PREFIX search response incomplete, count-mismatched, or above 1000; using repository-wise fallback" >&2
  repos_json_file="$(mktemp "${TMPDIR:-/tmp}/pr-green-repos.XXXXXX")"
  repo_names_file="$(mktemp "${TMPDIR:-/tmp}/pr-green-repo-names.XXXXXX")"
  if ! gh api --paginate --slurp -X GET /orgs/jleechanorg/repos \
    -f type=all -f per_page=100 >"$repos_json_file"; then
    rm -f -- "$search_json_file" "$repos_json_file" "$repo_names_file"
    return 1
  fi
  if ! jq -n --arg cutoff "$since" --arg source repo_fallback \
    --argjson total "$search_total" \
    --argjson item_count "$search_items" --argjson complete "$search_complete" \
    --slurpfile pages "$search_json_file" \
    '{cutoff:$cutoff,source:$source,pages:$pages[0],total_count:$total,item_count:$item_count,incomplete_results:($complete|not)}' \
    >"$METRICS_DIR/discovery-${run_started}.json"; then
    rm -f -- "$search_json_file" "$repos_json_file" "$repo_names_file"
    return 1
  fi
  if ! jq -r '.[][]? | .name // empty' "$repos_json_file" >"$repo_names_file"; then
    rm -f -- "$search_json_file" "$repos_json_file" "$repo_names_file"
    return 1
  fi
  while IFS= read -r repo; do
    [[ -n "$repo" ]] || continue
    if ! pulls="$(gh api --paginate --slurp -X GET "/repos/jleechanorg/${repo}/pulls" \
      -f state=open -f per_page=100)"; then
      rm -f -- "$search_json_file" "$repos_json_file" "$repo_names_file"
      return 1
    fi
    if ! jq -r --arg repo "$repo" --arg since "$since" '
      .[][]?
      | select(((.state // "open") | ascii_downcase) == "open")
      | select(.updated_at >= $since and ((.draft // false) | not))
      | [$repo, .number, .title, .html_url, .updated_at]
      | @tsv
    ' <<<"$pulls"; then
      rm -f -- "$search_json_file" "$repos_json_file" "$repo_names_file"
      return 1
    fi
  done <"$repo_names_file"
  rm -f -- "$search_json_file" "$repos_json_file" "$repo_names_file"
}

prs="$(discover_prs | sort -t $'\t' -k5,5r -k1,1 -k2,2n -k4,4)"

# Persist the complete discovery set for the report/audit. Dispatch caps are
# intentionally separate from discovery: a capped repair pass must not pretend
# that later eligible PRs were not found.
discovery_file="$METRICS_DIR/discovery-${run_started}.tsv"
printf '%s\n' "$prs" > "$discovery_file"
discovered_count="$(awk 'NF {n++} END {print n+0}' "$discovery_file")"
echo "$LOG_PREFIX discovered=$discovered_count eligible_recent_non_draft_prs (dispatch cap=$MAX_PRS)"

# Rotate the bounded admission window from the last selected PR. A fixed
# updated-time ordering would repeatedly spend the cap on the same prefix and
# starve older eligible PRs; the cursor survives runs and naturally resets
# when its previous key is no longer in the discovery set.
selection_cursor_file="$METRICS_DIR/selection-cursor"
ordered_prs="$prs"
if [[ -n "$prs" ]]; then
  mapfile -t discovery_rows <<<"$prs"
  row_count="${#discovery_rows[@]}"
  start_index=0
  cursor_key=""
  [[ -s "$selection_cursor_file" ]] && cursor_key="$(head -n 1 "$selection_cursor_file")"
  if [[ -n "$cursor_key" ]]; then
    for ((i = 0; i < row_count; i++)); do
      IFS=$'\t' read -r cursor_repo cursor_number _ <<<"${discovery_rows[i]}"
      if [[ "$cursor_repo#$cursor_number" == "$cursor_key" ]]; then
        start_index=$(((i + 1) % row_count))
        break
      fi
    done
  fi
  if (( start_index > 0 )); then
    ordered_prs="$(
      for ((offset = 0; offset < row_count; offset++)); do
        index=$(((start_index + offset) % row_count))
        printf '%s\n' "${discovery_rows[index]}"
      done
    )"
  fi
fi

if [[ -z "$prs" ]]; then
  echo "$LOG_PREFIX no recently updated open PRs"
  exit 0
fi

dispatched=0
selected=0
reused=0
restored=0
busy_deferred=0
cooldown_deferred=0
admission_deferred=0
recovery_blocked=0
delivery_unconfirmed=0
fixed_confirmed=0

# outcomes.jsonl is the stable reporting contract. `verified` is true only
# after a fresh GitHub read proves a new head cleared the original blocker.
record_outcome() {
  local repo="$1" number="$2" url="$3" before="$4" after="$5" classification="$6" action="$7"
  local result verified head_before head_after native_ack_status=untracked
  case "$action" in
    reused|restored) native_ack_status=observed ;;
    delivery_unconfirmed) native_ack_status=missing ;;
  esac
  read -r result verified <<<"$(pr_green_outcome_result "$classification")"
  head_before="$(jq -r '.head_sha' <<<"$before")"
  head_after="$(jq -r '.head_sha' <<<"$after")"
  jq -cn --argjson ts "$(date +%s)" --argjson run_ts "$run_started" \
    --arg repo "$repo" --argjson number "$number" --arg url "$url" \
    --arg head_before "$head_before" --arg head_after "$head_after" \
    --argjson blocker_before "$before" --argjson blocker_after "$after" \
    --arg classification "$classification" --arg action "$action" \
    --arg result "$result" --argjson verified "$verified" --arg native_ack_status "$native_ack_status" \
    '{ts:$ts,run_ts:$run_ts,repo:$repo,number:$number,url:$url,head_before:$head_before,head_after:$head_after,blocker_before:$blocker_before,blocker_after:$blocker_after,classification:$classification,session_action:$action,result:$result,verified:$verified,native_ack_status:$native_ack_status,detail:($classification + "; " + $action)}' \
    >> "$METRICS_DIR/outcomes.jsonl"
  [[ "$classification" == "fixed_confirmed" ]] && fixed_confirmed=$((fixed_confirmed + 1))
  # `[[ ... ]] &&` returns 1 for ordinary non-fix outcomes.  This helper is
  # called under `set -e`, so explicitly keep recording a no-change/cooldown
  # result from aborting the whole sweep before its run metrics are persisted.
  return 0
}

reconcile_pr() {
  local repo="$1" number="$2" url="$3" before="$4" action="$5" after classification snapshot_path
  after="$(pr_green_fetch_live_state "$url" 2>/dev/null || true)"
  after="$(pr_green_apply_required_contract "$repo" "$after" "$url")"
  [[ -n "$after" ]] || { echo "$LOG_PREFIX unable to re-read $repo#$number after $action" >&2; return 1; }
  classification="$(pr_green_classify_outcome "$before" "$after")"
  record_outcome "$repo" "$number" "$url" "$before" "$after" "$classification" "$action"
  snapshot_path="$(pr_green_snapshot_path "$STATE_DIR" "$repo" "$number")"
  if [[ "$classification" == "fixed_confirmed" ]]; then
    # Keep the original pre-dispatch blocker for every intermediate result.
    # Replacing it with a pending/blocked replacement head would make the later
    # green read look like `no_change`, losing the fix.
    rm -f -- "$snapshot_path"
  fi
  echo "$LOG_PREFIX outcome $repo#$number $classification ($action)"
}

# Register only a dedicated, existing git checkout with the Go AO project API.
# `ao start <URL>` belongs to the interactive project-start workflow and does
# not register the project identity required by `ao spawn --project`.
pr_green_register_project() {
  local repo="$1" project_id="$2" project_path origin expected
  project_path="$AO_PROJECT_ROOT/$repo"
  mkdir -p "$AO_PROJECT_ROOT"
  if [[ ! -d "$project_path/.git" ]]; then
    [[ ! -e "$project_path" ]] || return 1
    git clone --quiet --no-checkout \
      "https://github.com/jleechanorg/${repo}.git" "$project_path" >/dev/null 2>&1 || return 1
  fi
  git -C "$project_path" rev-parse --is-inside-work-tree >/dev/null 2>&1 || return 1
  origin="$(git -C "$project_path" remote get-url origin 2>/dev/null || true)"
  expected="https://github.com/jleechanorg/${repo}"
  case "${origin%.git}" in
    "$expected"|"git@github.com:jleechanorg/${repo}") ;;
    *) return 1 ;;
  esac
  ao project add --id "$project_id" --path "$project_path" >/dev/null 2>&1
}

while IFS=$'\t' read -r repo number title url updated; do
  [[ -n "$repo" && -n "$number" ]] || continue
  : "$updated" # retained from discovery for the audit TSV ordering
  live_state="$(pr_green_fetch_live_state "$url" 2>/dev/null || true)"
  live_state="$(pr_green_apply_required_contract "$repo" "$live_state" "$url")"
  if [[ -z "$live_state" ]]; then
    analysis_failed=$((analysis_failed + 1))
    echo "$LOG_PREFIX unable to inspect $repo#$number" >&2
    continue
  fi
  analyzed=$((analyzed + 1))
  mergeable="$(jq -r '.mergeability // (if .conflicting then "CONFLICTING" else "MERGEABLE" end)' <<<"$live_state")"
  failures="$(jq -r '.failed_checks | join(",")' <<<"$live_state")"
  previous_snapshot="$(pr_green_read_snapshot "$STATE_DIR" "$repo" "$number" || true)"
  if [[ -n "$previous_snapshot" ]]; then
    reconcile_pr "$repo" "$number" "$url" "$previous_snapshot" reconciled || true
  fi
  if [[ "$mergeable" == "UNKNOWN" ]]; then
    echo "$LOG_PREFIX skip $repo#$number (mergeability unknown; pending)"
    continue
  fi
  if [[ "$mergeable" != "CONFLICTING" && -z "$failures" ]]; then
    echo "$LOG_PREFIX skip $repo#$number (no conflict or failed check)"
    continue
  fi
  actionable=$((actionable + 1))
  if [[ "$repo" == "agent-orchestrator" || "$repo" == "agent-orchestrator-golang" ]]; then
    record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" no_change authorization_excluded
    echo "$LOG_PREFIX authorization excludes AO repository mutation for $repo#$number"
    continue
  fi
  if pr_green_same_head_cooldown_applies "$METRICS_DIR/outcomes.jsonl" "$repo" "$number" "$live_state" "$(date +%s)" "${PR_GREEN_SAME_HEAD_COOLDOWN_SECONDS:-28800}"; then
    cooldown_deferred=$((cooldown_deferred + 1))
    record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" no_change cooldown_deferred
    echo "$LOG_PREFIX unchanged blocker cooldown for $repo#$number; no AO/Codex inference"
    continue
  fi
  if (( selected >= MAX_PRS )); then
    echo "$LOG_PREFIX cap reached ($MAX_PRS); deferring $repo#$number to the next run"
    continue
  fi
  selected=$((selected + 1))
  cursor_tmp="${selection_cursor_file}.tmp.$$"
  printf '%s#%s\n' "$repo" "$number" >"$cursor_tmp"
  mv -- "$cursor_tmp" "$selection_cursor_file"
  echo "$LOG_PREFIX selected $repo#$number"

  prompt="$(cat <<EOF
Work on ${url} in ${repo}. This is an automated daily repair pass for a PR updated in the last ${WINDOW_HOURS} hours. Inspect the exact current PR head and base first.

MANDATORY INTEGRATION RULES
- Distinguish textual Git conflicts, generated-file/checksum conflicts, post-merge test failures, and genuine product-policy disagreements.
- A post-merge test failure is not automatically product ambiguity. Determine whether it protects user-visible PR behavior or only an implementation/file layout that the current base has superseded.
- Preserve the PR user-visible behavior while adapting stale implementation and tests to the current base architecture.
- A bounded integration repair may edit production code and tests together. Keep the scope limited to the PR behavior and the current-base contract.
- Regenerate derived manifests and checksums last. Do this after source and test integration is complete.
- Stop only when repository evidence leaves two or more genuinely plausible user-visible behaviors. Stop only if choosing between them would materially change product behavior, and report that exact blocker.

SAFETY AND DELIVERY
- Never merge the PR.
- Never rebase published history or rewrite history.
- Never force-push.
- Never change credentials.
- Never weaken tests merely to make them pass.
- Never manufacture an empty or no-op commit, bypass hooks, or wrap/replace push tools to obtain a receipt or satisfy delivery metrics.
- Run the narrowest relevant tests covering both the PR behavior and current-base contract, then the repository's required checks.
- Commit with an explicit message. Push normally only after the integrated tests and required checks are green.
- Before any push, invoke the job-owned receipt helper exactly as follows; do not use a bare git push:
  session_id="\${AO_SESSION_ID:-\${AGENT_ORCHESTRATOR_SESSION_ID:-}}"
  [[ -n "\$session_id" ]] || { echo 'exact AO session id unavailable; refusing push' >&2; exit 1; }
  before_sha="\$(gh pr view "https://github.com/jleechanorg/${repo}/pull/${number}" --json headRefOid --jq .headRefOid)"
  after_sha="\$(git rev-parse HEAD)"
  "${PUSH_RECEIPT_HELPER}" --repo "${repo}" --number "${number}" --session-id "\$session_id" \
    --before-sha "\$before_sha" --after-sha "\$after_sha" \
    --commit-url "https://github.com/jleechanorg/${repo}/commit/\$after_sha" \
    --worktree "\$PWD" --ledger "${METRICS_DIR}/push-receipts.jsonl"
- The helper performs the normal push and records the verified receipt. Never invent or copy an AO/native session id.
- If the push succeeds but receipt/verification remains pending or unavailable, preserve the exact before/after SHAs and logs, do not create another commit or push solely to obtain a metric/receipt, and report verification pending.

Current signals: mergeable=${mergeable:-unknown}; failing_checks=${failures:-none}.
EOF
)"

  if [[ "$DRY_RUN" == "1" ]]; then
    printf '%s would dispatch %s#%s (%s)\n' "$LOG_PREFIX" "$repo" "$number" "$title"
    continue
  fi

  # Save the first blocker/head snapshot immediately before contacting AO. A
  # replacement head may still be blocked on a later scan; keep the original
  # baseline until a green read confirms the repair instead of resetting it to
  # the replacement and losing the eventual fix accounting.
  if [[ -z "$(pr_green_read_snapshot "$STATE_DIR" "$repo" "$number" || true)" ]]; then
    pr_green_write_snapshot "$STATE_DIR" "$repo" "$number" "$live_state"
  fi

  # Reuse an already configured AO project. For a new repo, clone it into a
  # dedicated non-repository directory before registering it; never clone into
  # the scheduler's working directory.
  project_id="$repo"
  [[ "$repo" == "worldarchitect.ai" ]] && project_id="worldarchitect.ai"
  session_name="pr-${number}"
  session_action=""
  if ! pr_green_ensure_codex_scope "$project_id"; then
    # A project may not have been registered yet. Register it into AO's
    # project registry, then apply and verify the complete preserved config
    # before any session is reused, restored, or spawned.
    if ! ao project get "$project_id" --json >/dev/null 2>&1; then
      pr_green_register_project "$repo" "$project_id" || true
    fi
    if ! pr_green_ensure_codex_scope "$project_id"; then
      echo "$LOG_PREFIX failed to establish project-scoped Codex account for $repo#$number" >&2
      record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" dispatch_failed project_scope_failed
      continue
    fi
  fi
  existing_session_record="$(pr_green_session_record "$project_id" "$number" 2>/dev/null || true)"
  existing_session_terminated="$(jq -r 'if (.isTerminated // false) then "true" else "false" end' <<<"${existing_session_record:-null}" 2>/dev/null || printf 'false')"
  pending_delivery_status="$(pr_green_delivery_pending_status "$project_id" "$number" 2>/dev/null || true)"
  needs_new_admission=1
  if [[ -n "$existing_session_record" && "$existing_session_terminated" == false ]]; then
    needs_new_admission=0
  elif [[ -z "$existing_session_record" && "$pending_delivery_status" == pending ]]; then
    # Let the delivery/reuse helper resolve an existing durable request; it is
    # not a new AO worker admission.
    needs_new_admission=0
  fi
  admission_lock_held=0
  if (( needs_new_admission == 1 )); then
    if ! flock -n 8; then
      admission_deferred=$((admission_deferred + 1))
      record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" no_change admission_cap_deferred
      echo "$LOG_PREFIX another bounded run holds the admission lock; deferring $repo#$number" >&2
      continue
    fi
    admission_lock_held=1
    if ! pr_green_admission_available; then
      admission_deferred=$((admission_deferred + 1))
      record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" no_change admission_cap_deferred
      pr_green_release_admission_lock
      echo "$LOG_PREFIX active-session cap reached ($GLOBAL_ACTIVE_CAP); deferring new admission for $repo#$number" >&2
      continue
    fi
  fi
  if session_action="$(pr_green_reuse_session "$project_id" "$number" "$prompt")"; then
    case "$session_action" in
      restored)
      attempted=$((attempted + 1))
      restored=$((restored + 1))
      pr_green_release_admission_lock
      echo "$LOG_PREFIX restored and reused AO session for $repo#$number"
        ;;
      busy_deferred)
      busy_deferred=$((busy_deferred + 1))
      pr_green_release_admission_lock
      echo "$LOG_PREFIX AO session for $repo#$number is visibly busy; deferred without queuing a prompt"
        ;;
      *)
        attempted=$((attempted + 1))
        reused=$((reused + 1))
      pr_green_release_admission_lock
      echo "$LOG_PREFIX reused AO session for $repo#$number"
      ;;
    esac
    reconcile_pr "$repo" "$number" "$url" "$live_state" "$session_action" || true
    continue
  else
    session_reuse_rc=$?
    if [[ "$session_reuse_rc" -eq 4 ]]; then
      delivery_unconfirmed=$((delivery_unconfirmed + 1))
      pr_green_release_admission_lock
      record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" dispatch_failed delivery_unconfirmed || true
      echo "$LOG_PREFIX delivery unconfirmed for $repo#$number; duplicate spawn suppressed" >&2
      continue
    fi
    if [[ "$session_reuse_rc" -eq 3 ]]; then
      recovery_blocked=$((recovery_blocked + 1))
      pr_green_release_admission_lock
      record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" dispatch_failed recovery_blocked || true
      echo "$LOG_PREFIX native recovery blocked for $repo#$number; duplicate spawn suppressed" >&2
      continue
    fi
    if [[ "$session_reuse_rc" -eq 2 ]]; then
      attempted=$((attempted + 1))
      pr_green_release_admission_lock
      echo "$LOG_PREFIX existing AO session for $repo#$number rejected update; skipping duplicate spawn" >&2
      reconcile_pr "$repo" "$number" "$url" "$live_state" reuse_rejected || true
      continue
    fi
  fi
  if (( admission_lock_held == 0 )); then
    if ! flock -n 8; then
      admission_deferred=$((admission_deferred + 1))
      record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" no_change admission_cap_deferred
      echo "$LOG_PREFIX another bounded run holds the admission lock before spawn; deferring $repo#$number" >&2
      continue
    fi
    admission_lock_held=1
  fi
  if ! pr_green_admission_available; then
    admission_deferred=$((admission_deferred + 1))
    record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" no_change admission_cap_deferred
    pr_green_release_admission_lock
    echo "$LOG_PREFIX active-session cap reached before spawn ($GLOBAL_ACTIVE_CAP); deferring $repo#$number" >&2
    continue
  fi
  # AO serializes spawns per project; mirror that lock so candidates are
  # deferred instead of producing concurrent-spawn refusals.
  mkdir -p "$AO_SPAWN_LOCK_DIR"
  ao_spawn_lock="$AO_SPAWN_LOCK_DIR/jleechanorg-pr-green-ao-${project_id}.lock"
  spawn_cmd=(flock -n "$ao_spawn_lock" ao spawn --project "$project_id" --claim-pr "$number" --name "$session_name" --harness codex --prompt "$prompt")
  attempted=$((attempted + 1))
  spawn_err="$(mktemp)"
  # The Go AO CLI creates and claims the worker, then returns. Wait for that
  # command while the global admission lock is held; never infer dispatch from
  # a still-running process.
  "${spawn_cmd[@]}" >"$spawn_err" 2>&1 &
  spawn_pid=$!
  spawn_rc=0
  wait "$spawn_pid" || spawn_rc=$?
  if [[ -s "$spawn_err" ]]; then
    cat "$spawn_err" >&2
    # Require both the acknowledged output and the project-scoped durable row.
    if pr_green_spawn_output_is_success "$spawn_err"; then
      if pr_green_session_record "$project_id" "$number" >/dev/null 2>&1; then
        echo "$LOG_PREFIX dispatched $repo#$number (AO session acknowledged)"
        dispatched=$((dispatched + 1))
        pr_green_release_admission_lock
        rm -f "$spawn_err"
        reconcile_pr "$repo" "$number" "$url" "$live_state" dispatched || true
        continue
      fi
      echo "$LOG_PREFIX AO spawn acknowledged but durable session row was not observable for $repo#$number" >&2
      record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" dispatch_failed spawn_unverified
      pr_green_release_admission_lock
      rm -f "$spawn_err"
      continue
    fi
    echo "$LOG_PREFIX AO spawn returned an unverified acknowledgement for $repo#$number; no duplicate retry" >&2
    record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" dispatch_failed spawn_unverified
    pr_green_release_admission_lock
    rm -f "$spawn_err"
    continue
  fi
  record_outcome "$repo" "$number" "$url" "$live_state" "$live_state" dispatch_failed spawn_failed
  echo "$LOG_PREFIX AO spawn failed for $repo#$number (rc=$spawn_rc)" >&2
  pr_green_release_admission_lock
  rm -f "$spawn_err"
done <<< "$ordered_prs"

jq -n --argjson ts "$run_started" --argjson analyzed "$analyzed" \
  --argjson analysis_failed "$analysis_failed" \
  --argjson discovered "$discovered_count" \
  --argjson actionable "$actionable" --argjson selected "$selected" \
  --argjson attempted "$attempted" \
  --argjson dispatched "$dispatched" --argjson reused "$reused" \
  --argjson restored "$restored" --argjson busy_deferred "$busy_deferred" \
  --argjson cooldown_deferred "$cooldown_deferred" \
  --argjson admission_deferred "$admission_deferred" \
  --argjson recovery_blocked "$recovery_blocked" \
  --argjson delivery_unconfirmed "$delivery_unconfirmed" \
  --argjson fixed_confirmed "$fixed_confirmed" \
  '{ts:$ts, discovered:$discovered, analyzed:$analyzed, analysis_failed:$analysis_failed, actionable:$actionable, selected:$selected, attempted:$attempted, dispatched:$dispatched, reused:$reused, restored:$restored, busy_deferred:$busy_deferred, cooldown_deferred:$cooldown_deferred, admission_deferred:$admission_deferred, recovery_blocked:$recovery_blocked, delivery_unconfirmed:$delivery_unconfirmed, fixed_confirmed:$fixed_confirmed}' \
  >> "$METRICS_DIR/runs.jsonl"
echo "$LOG_PREFIX summary analyzed=$analyzed actionable=$actionable selected=$selected attempted=$attempted dispatched=$dispatched reused=$reused restored=$restored busy_deferred=$busy_deferred cooldown_deferred=$cooldown_deferred recovery_blocked=$recovery_blocked delivery_unconfirmed=$delivery_unconfirmed fixed_confirmed=$fixed_confirmed"
