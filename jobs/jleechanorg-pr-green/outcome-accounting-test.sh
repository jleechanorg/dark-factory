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

green='{"head_sha":"after","conflicting":false,"failed_checks":[],"pending_checks":false}'
assert_eq "$(pr_green_classify_outcome "$before" "$green")" fixed_confirmed
assert_eq "$(pr_green_outcome_result fixed_confirmed)" 'fixed true'
assert_eq "$(pr_green_outcome_result pushed_ci_pending)" 'in_progress false'

raw_pr='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{"name":"lint","conclusion":"FAILURE","status":"COMPLETED"},{"name":"test","conclusion":null,"status":"IN_PROGRESS"}]}'
state="$(pr_green_state_from_pr_json <<<"$raw_pr")"
assert_eq "$(jq -r '.head_sha' <<<"$state")" after
assert_eq "$(jq -r '.failed_checks | join(",")' <<<"$state")" lint
assert_eq "$(jq -r '.pending_checks' <<<"$state")" true

state_dir="$(mktemp -d)"
trap 'rm -rf "$state_dir"' EXIT
pr_green_write_snapshot "$state_dir" worldarchitect.ai 123 "$before"
assert_eq "$(pr_green_read_snapshot "$state_dir" worldarchitect.ai 123)" "$before"

printf 'outcome accounting tests passed\n'
