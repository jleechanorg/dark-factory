#!/usr/bin/env bash
set -euo pipefail

job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=outcome-accounting.sh
source "$job_dir/outcome-accounting.sh"

assert_eq() {
  [[ "$1" == "$2" ]] || { printf 'assertion failed: %s != %s\n' "$1" "$2" >&2; exit 1; }
}

before='{"head_sha":"before","conflicting":true,"failed_checks":["unit"],"pending_checks":false}'
same='{"head_sha":"before","conflicting":true,"failed_checks":["unit"],"pending_checks":false}'
assert_eq "$(pr_green_classify_outcome "$before" "$same")" no_change

pending='{"head_sha":"after","conflicting":false,"failed_checks":[],"pending_checks":true}'
assert_eq "$(pr_green_classify_outcome "$before" "$pending")" pushed_ci_pending

still_blocked='{"head_sha":"after","conflicting":false,"failed_checks":["unit"],"pending_checks":false}'
assert_eq "$(pr_green_classify_outcome "$before" "$still_blocked")" pushed_still_blocked

green='{"head_sha":"after","conflicting":false,"failed_checks":[],"pending_checks":false,"check_count":1,"successful_completed_checks":1}'
assert_eq "$(pr_green_classify_outcome "$before" "$green")" fixed_confirmed
assert_eq "$(pr_green_outcome_result fixed_confirmed)" 'fixed true'
assert_eq "$(pr_green_outcome_result pushed_ci_pending)" 'in_progress false'

raw_pr='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{"name":"lint","conclusion":"FAILURE","status":"COMPLETED"},{"name":"test","conclusion":null,"status":"IN_PROGRESS"}]}'
state="$(pr_green_state_from_pr_json <<<"$raw_pr")"
assert_eq "$(jq -r '.head_sha' <<<"$state")" after
assert_eq "$(jq -r '.failed_checks | join(",")' <<<"$state")" lint
assert_eq "$(jq -r '.pending_checks' <<<"$state")" true

# A new head can be returned before GitHub has registered any check contexts.
# For a CI-origin repair that is a pending verification state, not a fix.
ci_before='{"head_sha":"before","conflicting":false,"failed_checks":["unit"],"pending_checks":false}'
empty_rollup='{"head_sha":"after","conflicting":false,"failed_checks":[],"pending_checks":false,"check_count":0,"successful_completed_checks":0}'
assert_eq "$(pr_green_classify_outcome "$ci_before" "$empty_rollup")" pushed_ci_pending

# CI-origin repairs need a completed successful context on the replacement head.
registered_green='{"head_sha":"after","conflicting":false,"failed_checks":[],"pending_checks":false,"check_count":1,"successful_completed_checks":1}'
assert_eq "$(pr_green_classify_outcome "$ci_before" "$registered_green")" fixed_confirmed

# A conflict-only repair does not manufacture a CI requirement.
conflict_before='{"head_sha":"before","conflicting":true,"failed_checks":[],"pending_checks":false}'
assert_eq "$(pr_green_classify_outcome "$conflict_before" "$empty_rollup")" fixed_confirmed

# REST-shaped status contexts count as completed successful evidence too.
status_context_pr='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{"context":"ci/build","state":"SUCCESS","status":"COMPLETED"}]}'
state="$(pr_green_state_from_pr_json <<<"$status_context_pr")"
assert_eq "$(jq -r '.check_count' <<<"$state")" 1
assert_eq "$(jq -r '.successful_completed_checks' <<<"$state")" 1

# GitHub retains cancelled check-runs from superseded duplicate workflows in
# statusCheckRollup. A completed successful replacement for the same check on
# this exact PR head is not a CI failure and must not consume a repair worker.
superseded_cancelled='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{"name":"test","conclusion":"CANCELLED","status":"COMPLETED"},{"name":"test","conclusion":"SUCCESS","status":"COMPLETED"}]}'
state="$(pr_green_state_from_pr_json <<<"$superseded_cancelled")"
assert_eq "$(jq -r '.failed_checks | join(",")' <<<"$state")" ''

state_dir="$(mktemp -d)"
trap 'rm -rf "$state_dir"' EXIT
pr_green_write_snapshot "$state_dir" worldarchitect.ai 123 "$before"
assert_eq "$(pr_green_read_snapshot "$state_dir" worldarchitect.ai 123)" "$before"

# A concrete blocker that was already sent to a worker at this exact head must
# not consume another Codex turn every 30 minutes.  Reconciliation records are
# deliberately ignored as anchors because the scheduler emits those on every
# scan; otherwise the cooldown would renew forever.
outcomes_file="$state_dir/outcomes.jsonl"
cat >"$outcomes_file" <<'EOF'
{"ts":900,"repo":"worldarchitect.ai","number":9941,"head_after":"same-head","blocker_after":{"conflicting":true,"failed_checks":[]},"classification":"no_change","session_action":"reused"}
{"ts":990,"repo":"worldarchitect.ai","number":9941,"head_after":"same-head","blocker_after":{"conflicting":true,"failed_checks":[]},"classification":"no_change","session_action":"reconciled"}
EOF
current_blocker='{"head_sha":"same-head","conflicting":true,"failed_checks":[]}'
if ! pr_green_same_head_cooldown_applies "$outcomes_file" worldarchitect.ai 9941 "$current_blocker" 1000 28800; then
  printf 'expected unchanged concrete blocker to enter cooldown\n' >&2
  exit 1
fi

# A head or meaningful blocker change is fresh work, and an awaiting-CI result
# remains eligible for reconciliation rather than being suppressed.
changed_head='{"head_sha":"new-head","conflicting":true,"failed_checks":[]}'
if pr_green_same_head_cooldown_applies "$outcomes_file" worldarchitect.ai 9941 "$changed_head" 1000 28800; then
  printf 'head change must bypass cooldown\n' >&2
  exit 1
fi
changed_blocker='{"head_sha":"same-head","conflicting":false,"failed_checks":["unit"]}'
if pr_green_same_head_cooldown_applies "$outcomes_file" worldarchitect.ai 9941 "$changed_blocker" 1000 28800; then
  printf 'blocker change must bypass cooldown\n' >&2
  exit 1
fi
printf '%s\n' '{"ts":999,"repo":"worldarchitect.ai","number":9941,"head_after":"same-head","blocker_after":{"conflicting":true,"failed_checks":[]},"classification":"pushed_ci_pending","session_action":"reused"}' >"$outcomes_file"
if pr_green_same_head_cooldown_applies "$outcomes_file" worldarchitect.ai 9941 "$current_blocker" 1000 28800; then
  printf 'pushed_ci_pending must bypass cooldown\n' >&2
  exit 1
fi

printf 'outcome accounting tests passed\n'
