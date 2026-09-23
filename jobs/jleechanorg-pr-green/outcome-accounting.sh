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

# Classify an exact-current-head state against the blocker snapshot saved before
# AO was contacted. A changed head alone is never a successful repair.
pr_green_classify_outcome() {
  local before="$1" after="$2"
  jq -nr --argjson before "$before" --argjson after "$after" '
    if $before.head_sha == $after.head_sha then "no_change"
    elif ($after.conflicting or (($after.failed_checks | length) > 0)) then "pushed_still_blocked"
    elif $after.pending_checks then "pushed_ci_pending"
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
# terminal failures as blockers; pending checks are reported separately.
pr_green_state_from_pr_json() {
  jq -c '
    def failed: ["FAILURE","FAILED","CANCELLED","TIMED_OUT","ACTION_REQUIRED"];
    {
      head_sha: (.headRefOid // .headRefName // ""),
      conflicting: ((.mergeable == "CONFLICTING") or (.mergeStateStatus == "DIRTY") or (.mergeStateStatus == "CONFLICTING")),
      failed_checks: [(.statusCheckRollup // [])[]?
        | select(((.conclusion // "") | ascii_upcase) as $c | failed | index($c))
        | (.name // .workflowName // "unnamed")],
      pending_checks: any((.statusCheckRollup // [])[]?;
        ((.conclusion // "") == "") and ((.status // "") | ascii_upcase) != "COMPLETED")
    }'
}

pr_green_fetch_live_state() {
  local url="$1" payload
  payload="$(gh pr view "$url" --json headRefOid,mergeable,mergeStateStatus,statusCheckRollup 2>/dev/null)" || return 1
  pr_green_state_from_pr_json <<<"$payload"
}
