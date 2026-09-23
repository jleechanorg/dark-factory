#!/usr/bin/env bash

# Durable, evidence-based accounting for the PR-green scheduler. Dispatching an
# agent is not a fix: only a later GitHub read of a new PR head can confirm it.

pr_green_snapshot_path() {
  local state_dir="$1" repo="$2" pr_number="$3"
  printf '%s/%s-%s.json\n' "$state_dir" "${repo//\//_}" "$pr_number"
}

pr_green_write_snapshot() {
  local state_dir="$1" repo="$2" pr_number="$3" snapshot="$4"
  local path tmp
  path="$(pr_green_snapshot_path "$state_dir" "$repo" "$pr_number")"
  mkdir -p "$state_dir"
  tmp="${path}.tmp.$$"
  printf '%s\n' "$snapshot" >"$tmp"
  mv "$tmp" "$path"
}

pr_green_read_snapshot() {
  local path
  path="$(pr_green_snapshot_path "$1" "$2" "$3")"
  [[ -f "$path" ]] && cat "$path"
}

# Return success when an unchanged, concretely blocked PR was already offered
# to a worker recently.  The durable outcomes log is the source of truth: a
# reconciliation entry is emitted on every scan, so it must never renew this
# cooldown.  A head/blocker change and pending CI verification both remain
# fresh work.
pr_green_same_head_cooldown_applies() {
  local outcomes_file="$1" repo="$2" pr_number="$3" current_state="$4" now="$5" cooldown_seconds="$6"
  [[ -s "$outcomes_file" ]] || return 1

  jq -ne --arg repo "$repo" --argjson number "$pr_number" \
    --argjson current "$current_state" --argjson now "$now" --argjson cooldown "$cooldown_seconds" '
      def signature($state): {
        conflicting: ($state.conflicting // false),
        failed_checks: (($state.failed_checks // []) | sort)
      };
      [inputs
       | select(.repo == $repo and .number == $number)
       | select(.session_action != "reconciled" and .session_action != "cooldown_deferred")]
      | sort_by(.ts // 0)
      | last as $latest
      | $latest != null
        and (($latest.ts // 0) >= ($now - $cooldown))
        and ($latest.classification | IN("no_change", "pushed_still_blocked"))
        and ($latest.head_after == $current.head_sha)
        and (signature($latest.blocker_after) == signature($current))
    ' "$outcomes_file" >/dev/null
}

# Classify an exact-current-head state against the blocker snapshot saved before
# AO was contacted. A changed head alone is never a successful repair.
pr_green_classify_outcome() {
  local before="$1" after="$2"
  jq -nr --argjson before "$before" --argjson after "$after" '
    if $before.head_sha == $after.head_sha then "no_change"
    elif ($after.conflicting or (($after.failed_checks | length) > 0)) then "pushed_still_blocked"
    elif $after.pending_checks or $after.verification_pending
      or ((($after.required_checks_missing // []) | length) > 0)
    then "pushed_ci_pending"
    # A head pushed to repair a CI failure can briefly have an empty rollup
    # while GitHub registers the new check-runs.  Do not treat that gap as a
    # green CI result.  Conflict-only repairs are intentionally exempt: they
    # may be complete before a repository has any checks at all.
    elif (($before.failed_checks | length) > 0)
      and (((($after.check_count // 0) == 0) or (($after.successful_completed_checks // 0) == 0)))
    then "pushed_ci_pending"
    else "fixed_confirmed"
    end'
}

# Print the reporting result and whether it has been independently verified.
pr_green_outcome_result() {
  case "$1" in
    fixed_confirmed) printf '%s\n' 'fixed true' ;;
    pushed_still_blocked) printf '%s\n' 'blocked false' ;;
    pushed_ci_pending) printf '%s\n' 'in_progress false' ;;
    no_change) printf '%s\n' 'no_change false' ;;
    *) printf '%s\n' 'dispatch_failed false' ;;
  esac
}

# Emit a compact state from one authoritative gh pr view response. It supports
# the REST-shaped status rollup returned by gh and intentionally treats only
# actionable terminal failures as blockers; pending checks are reported
# separately.  Counts retain whether GitHub has registered and completed
# successful evidence on this exact head. CANCELLED is excluded because GitHub
# keeps superseded workflow runs in the rollup after their successful
# replacement completes.
pr_green_state_from_pr_json() {
  jq -c '
    def failed: ["FAILURE","FAILED","ERROR","TIMED_OUT","ACTION_REQUIRED","STARTUP_FAILURE"];
    def state: ((.conclusion // .state // "") | ascii_upcase);
    def status: ((.status // "") | ascii_upcase);
    def completed: (status == "" or status == "COMPLETED");
    {
      head_sha: (.headRefOid // .headRefName // ""),
      mergeability: ((.mergeable // "") | ascii_upcase),
      conflicting: ((.mergeable == "CONFLICTING") or (.mergeStateStatus == "DIRTY") or (.mergeStateStatus == "CONFLICTING")),
      failed_checks: [(.statusCheckRollup // [])[]?
        | select(state as $state | failed | index($state))
        | (.name // .workflowName // "unnamed")],
      check_count: [(.statusCheckRollup // [])[]?] | length,
      successful_completed_checks: [(.statusCheckRollup // [])[]?
        | select(state == "SUCCESS" and completed)] | length,
      check_statuses: (reduce ((.statusCheckRollup // [])[]?) as $check ({};
        .[($check.name // $check.context // "")] = (($check.conclusion // $check.state // "") | ascii_upcase))),
      pending_checks: (any((.statusCheckRollup // [])[]?;
        (state == "" and status != "COMPLETED")
        or (state | IN("PENDING", "EXPECTED", "QUEUED", "IN_PROGRESS", "REQUESTED"))
      ) or (((.mergeable // "") | ascii_upcase) == "UNKNOWN")
        or (((.mergeStateStatus // "") | ascii_upcase) == "UNKNOWN"))
    }'
}

pr_green_apply_required_contract() {
  local repo="$1" state="$2"
  if [[ "$repo" != "worldarchitect.ai" ]]; then
    jq -c '. + {verification_pending:true,verification_reason:"required_check_contract_unknown"}' <<<"$state"
    return
  fi
  jq -c '. as $root | . + {required_checks:["Green Gate","Tests Required Gate"]}
    | .required_checks_missing=[.required_checks[] | select(($root.check_statuses[.]) != "SUCCESS")]' <<<"$state"
}

pr_green_fetch_live_state() {
  local url="$1" payload
  payload="$(gh pr view "$url" --json headRefOid,mergeable,mergeStateStatus,statusCheckRollup 2>/dev/null)" || return 1
  pr_green_state_from_pr_json <<<"$payload"
}
