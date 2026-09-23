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

unknown='{"headRefOid":"after","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","statusCheckRollup":[]}'
assert_eq "$(jq -r '.pending_checks' <<<"$(pr_green_state_from_pr_json <<<"$unknown")")" true
gates='{"head_sha":"after","conflicting":false,"failed_checks":[],"pending_checks":false,"check_statuses":{"Green Gate":"SUCCESS","Tests Required Gate":"SUCCESS"}}'
assert_eq "$(jq -r '.required_checks_missing | length' <<<"$(pr_green_apply_required_contract worldarchitect.ai "$gates")")" 0
missing_gate='{"head_sha":"after","conflicting":false,"failed_checks":[],"pending_checks":false,"check_statuses":{"Green Gate":"SUCCESS"}}'
missing_state="$(pr_green_apply_required_contract worldarchitect.ai "$missing_gate")"
assert_eq "$(jq -r '.required_checks_missing | join(",")' <<<"$missing_state")" 'Tests Required Gate'
assert_eq "$(pr_green_classify_outcome "$before" "$missing_state")" pushed_ci_pending

error_check='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{"name":"tests","conclusion":"ERROR","status":"COMPLETED"}]}'
state="$(pr_green_state_from_pr_json <<<"$error_check")"
assert_eq "$(jq -r '.failed_checks | join(",")' <<<"$state")" tests

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

# A newer successful attempt for the same logical CheckRun must retire an older
# failure regardless of the order GitHub returns the rollup rows.
failure_then_success='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[
  {"name":"unit","workflowName":"CI","conclusion":"SUCCESS","status":"COMPLETED","startedAt":"2026-09-23T08:05:00Z","completedAt":"2026-09-23T08:06:00Z","detailsUrl":"https://github.com/example/runs/2"},
  {"name":"unit","workflowName":"CI","conclusion":"FAILURE","status":"COMPLETED","startedAt":"2026-09-23T08:00:00Z","completedAt":"2026-09-23T08:01:00Z","detailsUrl":"https://github.com/example/runs/1"}
]}'
state="$(pr_green_state_from_pr_json <<<"$failure_then_success")"
assert_eq "$(jq -r '.failed_checks | length' <<<"$state")" 0
assert_eq "$(jq -r '.check_count' <<<"$state")" 1
assert_eq "$(jq -r '.successful_completed_checks' <<<"$state")" 1
assert_eq "$(jq -r '.pending_checks' <<<"$state")" false

# Conversely, a newer failed attempt remains actionable even when the stale
# success appears after it in the payload.
success_then_failure='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[
  {"name":"unit","workflowName":"CI","conclusion":"FAILURE","status":"COMPLETED","startedAt":"2026-09-23T08:05:00Z","completedAt":"2026-09-23T08:06:00Z","detailsUrl":"https://github.com/example/runs/2"},
  {"name":"unit","workflowName":"CI","conclusion":"SUCCESS","status":"COMPLETED","startedAt":"2026-09-23T08:00:00Z","completedAt":"2026-09-23T08:01:00Z","detailsUrl":"https://github.com/example/runs/1"}
]}'
state="$(pr_green_state_from_pr_json <<<"$success_then_failure")"
assert_eq "$(jq -r '.failed_checks | join(",")' <<<"$state")" unit
assert_eq "$(jq -r '.successful_completed_checks' <<<"$state")" 0
assert_eq "$(jq -r '.pending_checks' <<<"$state")" false

# A newer pending retry retires an older failure but must keep verification
# pending until that retry reaches a terminal conclusion.
pending_retry='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[
  {"name":"unit","workflowName":"CI","conclusion":"","status":"QUEUED","startedAt":"2026-09-23T08:10:00Z","completedAt":"0001-01-01T00:00:00Z","detailsUrl":"https://github.com/example/runs/2"},
  {"name":"unit","workflowName":"CI","conclusion":"FAILURE","status":"COMPLETED","startedAt":"2026-09-23T08:00:00Z","completedAt":"2026-09-23T08:20:00Z","detailsUrl":"https://github.com/example/runs/1"}
]}'
state="$(pr_green_state_from_pr_json <<<"$pending_retry")"
assert_eq "$(jq -r '.failed_checks | length' <<<"$state")" 0
assert_eq "$(jq -r '.check_count' <<<"$state")" 1
assert_eq "$(jq -r '.successful_completed_checks' <<<"$state")" 0
assert_eq "$(jq -r '.pending_checks' <<<"$state")" true

# When attempt timestamps tie, prefer the provider's exact numeric identity,
# not lexical detailsUrl ordering (for example run/9 versus run/10).
numeric_identity_tie='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[
  {"name":"unit","workflowName":"CI","conclusion":"SUCCESS","status":"COMPLETED","startedAt":"2026-09-23T08:10:00Z","completedAt":"2026-09-23T08:11:00Z","detailsUrl":"https://github.com/example/repo/actions/runs/10/job/100"},
  {"name":"unit","workflowName":"CI","conclusion":"FAILURE","status":"COMPLETED","startedAt":"2026-09-23T08:10:00Z","completedAt":"2026-09-23T08:11:00Z","detailsUrl":"https://github.com/example/repo/actions/runs/9/job/999"}
]}'
state="$(pr_green_state_from_pr_json <<<"$numeric_identity_tie")"
assert_eq "$(jq -r '.failed_checks | length' <<<"$state")" 0
assert_eq "$(jq -r '.successful_completed_checks' <<<"$state")" 1

# Same display name from different workflows is not one attempt stream: a
# failure in either workflow remains actionable and the shared gate label is
# conservative rather than overwritten by success from the other workflow.
cross_workflow_same_name='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[
  {"name":"unit","workflowName":"Other CI","conclusion":"SUCCESS","status":"COMPLETED","startedAt":"2026-09-23T08:10:00Z","completedAt":"2026-09-23T08:11:00Z","detailsUrl":"https://github.com/example/repo/actions/runs/10/job/100"},
  {"name":"unit","workflowName":"CI","conclusion":"FAILURE","status":"COMPLETED","startedAt":"2026-09-23T08:10:00Z","completedAt":"2026-09-23T08:11:00Z","detailsUrl":"https://github.com/example/repo/actions/runs/9/job/999"}
]}'
state="$(pr_green_state_from_pr_json <<<"$cross_workflow_same_name")"
assert_eq "$(jq -r '.failed_checks | join(",")' <<<"$state")" unit
assert_eq "$(jq -r '.check_count' <<<"$state")" 2
assert_eq "$(jq -r '.check_statuses.unit' <<<"$state")" MIXED

# A queued retry may have neither useful timestamp (GitHub's zero completedAt
# sentinel included), but its newer Actions run identity still supersedes an
# older terminal failure.
queued_identity_only='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[
  {"name":"unit","workflowName":"CI","conclusion":"","status":"QUEUED","startedAt":"","completedAt":"0001-01-01T00:00:00Z","detailsUrl":"https://github.com/example/repo/actions/runs/10/job/100"},
  {"name":"unit","workflowName":"CI","conclusion":"FAILURE","status":"COMPLETED","startedAt":"2026-09-23T08:00:00Z","completedAt":"2026-09-23T08:20:00Z","detailsUrl":"https://github.com/example/repo/actions/runs/9/job/999"}
]}'
state="$(pr_green_state_from_pr_json <<<"$queued_identity_only")"
assert_eq "$(jq -r '.failed_checks | length' <<<"$state")" 0
assert_eq "$(jq -r '.pending_checks' <<<"$state")" true

# Opaque equal-rank attempts cannot be ordered safely; keep the context
# pending rather than letting payload order or lexical URL order decide.
opaque_tie='{"headRefOid":"after","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[
  {"name":"unit","workflowName":"CI","conclusion":"SUCCESS","status":"COMPLETED","startedAt":"2026-09-23T08:10:00Z","completedAt":"2026-09-23T08:11:00Z","detailsUrl":"https://ci.example/jobs/10"},
  {"name":"unit","workflowName":"CI","conclusion":"FAILURE","status":"COMPLETED","startedAt":"2026-09-23T08:10:00Z","completedAt":"2026-09-23T08:11:00Z","detailsUrl":"https://ci.example/jobs/9"}
]}'
state="$(pr_green_state_from_pr_json <<<"$opaque_tie")"
assert_eq "$(jq -r '.failed_checks | length' <<<"$state")" 0
assert_eq "$(jq -r '.successful_completed_checks' <<<"$state")" 0
assert_eq "$(jq -r '.pending_checks' <<<"$state")" true

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

# A replacement head that is still waiting on CI must keep the original
# pre-dispatch blocker as its snapshot baseline. Otherwise the later green
# read of that same replacement head is misclassified as no_change.
pending_before='{"head_sha":"old-head","conflicting":false,"failed_checks":["unit"],"pending_checks":false}'
pending_after='{"head_sha":"new-head","conflicting":false,"failed_checks":[],"pending_checks":true}'
assert_eq "$(pr_green_classify_outcome "$pending_before" "$pending_after")" pushed_ci_pending
pending_green='{"head_sha":"new-head","conflicting":false,"failed_checks":[],"pending_checks":false,"check_count":1,"successful_completed_checks":1}'
assert_eq "$(pr_green_classify_outcome "$pending_before" "$pending_green")" fixed_confirmed

# Required-check contract fixtures exercise the real API aggregation path with
# a shell-level GitHub boundary double. The state projection remains the code
# under test; the double only supplies complete, deterministic API responses.
pr_green_test_head_sha=after
pr_green_test_view_count=0
pr_green_test_classic='{"required_status_checks":{"contexts":[],"checks":[]}}'
pr_green_test_rules='[]'
pr_green_test_check_runs='[{"check_runs":[]}]'
pr_green_test_api_mode=ok
pr_green_test_head_mode=stable
gh() {
  local path
  if [[ "$1" == pr && "$2" == view ]]; then
    pr_green_test_view_count=$((pr_green_test_view_count + 1))
    local view_sha="$pr_green_test_head_sha"
    if [[ "$pr_green_test_head_mode" == move ]]; then
      view_sha=moved
    fi
    jq -cn --arg sha "$view_sha" '{headRefOid:$sha,baseRefName:"main",headRepository:{nameWithOwner:"jleechanorg/example-repo"}}'
    return 0
  fi
  if [[ "$1" == api ]]; then
    path="${*: -1}"
    case "$path" in
      */protection)
        if [[ "$pr_green_test_api_mode" == unknown ]]; then
          printf '%s\n' '{"message":"API unavailable"}'
          return 1
        fi
        if [[ "$pr_green_test_api_mode" == no_protection ]]; then
          printf '%s\n' '{"message":"Branch not protected"}'
          return 1
        fi
        printf '%s\n' "$pr_green_test_classic"
        return 0
        ;;
      */rules/branches/*)
        if [[ "$pr_green_test_api_mode" == unknown ]]; then
          printf '%s\n' '{"message":"rules unavailable"}'
          return 1
        fi
        printf '%s\n' "$pr_green_test_rules"
        return 0
        ;;
      */check-runs*)
        printf '%s\n' "$pr_green_test_check_runs"
        return 0
        ;;
    esac
  fi
  printf 'unexpected gh fixture call: %s\n' "$*" >&2
  return 1
}

contract_state='{"head_sha":"after","base_ref_name":"main","head_repo_name":"jleechanorg/example-repo","conflicting":false,"failed_checks":[],"pending_checks":false,"check_statuses":{"unit":"SUCCESS"}}'

# An unreadable protection/rules source must remain pending, rather than being
# interpreted as no required checks.
pr_green_test_api_mode=unknown
pr_green_test_head_mode=stable
pr_green_test_view_count=0
unknown_contract="$(pr_green_apply_required_contract example-repo "$contract_state" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.verification_pending' <<<"$unknown_contract")" true
assert_eq "$(jq -r '.verification_reason' <<<"$unknown_contract")" required_check_contract_unavailable

# A structured classic "Branch not protected" result is authoritative only
# when the rules endpoint is a readable array.
pr_green_test_api_mode=no_protection
pr_green_test_rules='[]'
pr_green_test_view_count=0
no_protection="$(pr_green_apply_required_contract example-repo "$contract_state" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.verification_pending // false' <<<"$no_protection")" false
assert_eq "$(jq -r '.required_checks | length' <<<"$no_protection")" 0

# Classic and ruleset requirements are additive, including an app-bound check.
pr_green_test_api_mode=ok
pr_green_test_classic='{"required_status_checks":{"contexts":["classic"],"checks":[{"context":"classic-app","app_id":42}]}}'
pr_green_test_rules='[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"rule","integration_id":42}]}}]'
pr_green_test_check_runs='[{"check_runs":[{"name":"classic-app","head_sha":"after","started_at":"2026-09-23T08:01:00Z","status":"completed","conclusion":"success","app":{"id":42}},{"name":"rule","head_sha":"after","started_at":"2026-09-23T08:02:00Z","status":"completed","conclusion":"success","app":{"id":42}}]}]'
pr_green_test_view_count=0
union_state="$(pr_green_apply_required_contract example-repo "$contract_state" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.required_checks | sort | join(",")' <<<"$union_state")" 'classic,classic-app,rule'

# A declared requirement with no successful exact-head evidence is pending.
pr_green_test_classic='{"required_status_checks":{"contexts":["required"],"checks":[]}}'
pr_green_test_rules='[]'
pr_green_test_check_runs='[{"check_runs":[]}]'
empty_status_state='{"head_sha":"after","base_ref_name":"main","head_repo_name":"jleechanorg/example-repo","conflicting":false,"failed_checks":[],"pending_checks":false,"check_statuses":{}}'
missing_state="$(pr_green_apply_required_contract example-repo "$empty_status_state" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.verification_pending' <<<"$missing_state")" true
assert_eq "$(jq -r '.verification_reason' <<<"$missing_state")" required_checks_pending
assert_eq "$(jq -r '.required_checks_missing | join(",")' <<<"$missing_state")" required

# A non-terminal exact-head check is also pending.
pending_status='{"head_sha":"after","base_ref_name":"main","head_repo_name":"jleechanorg/example-repo","conflicting":false,"failed_checks":[],"pending_checks":false,"check_statuses":{"required":"PENDING"}}'
pending_state="$(pr_green_apply_required_contract example-repo "$pending_status" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.verification_pending' <<<"$pending_state")" true
assert_eq "$(jq -r '.verification_reason' <<<"$pending_state")" required_checks_pending

# An app-bound requirement must match both name and app.id on an exact-head
# check-run; a successful rollup row without the expected app is insufficient.
pr_green_test_classic='{"required_status_checks":{"contexts":[],"checks":[{"context":"app-check","app_id":42}]}}'
pr_green_test_check_runs='[{"check_runs":[{"name":"app-check","status":"completed","conclusion":"success","app":{"id":99}}]}]'
wrong_app="$(pr_green_apply_required_contract example-repo "$empty_status_state" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.verification_pending' <<<"$wrong_app")" true
assert_eq "$(jq -r '.required_checks_missing | join(",")' <<<"$wrong_app")" app-check

# A newer failed attempt supersedes an older success, and a success from a
# different head cannot satisfy the app-bound exact-head requirement.
pr_green_test_check_runs='[{"check_runs":[
  {"name":"app-check","head_sha":"after","started_at":"2026-09-23T08:01:00Z","status":"completed","conclusion":"success","app":{"id":42}},
  {"name":"app-check","head_sha":"after","started_at":"2026-09-23T08:02:00Z","status":"completed","conclusion":"failure","app":{"id":42}},
  {"name":"app-check","head_sha":"other","started_at":"2026-09-23T08:03:00Z","status":"completed","conclusion":"success","app":{"id":42}}
]}]'
latest_bad="$(pr_green_apply_required_contract example-repo "$empty_status_state" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.verification_pending' <<<"$latest_bad")" true
assert_eq "$(jq -r '.required_checks_missing | join(",")' <<<"$latest_bad")" app-check

# Classic app_id=-1 explicitly allows any provider app.
pr_green_test_classic='{"required_status_checks":{"contexts":[],"checks":[{"context":"any-app","app_id":-1}]}}'
pr_green_test_check_runs='[{"check_runs":[{"name":"any-app","head_sha":"after","started_at":"2026-09-23T08:04:00Z","status":"completed","conclusion":"success","app":{"id":99}}]}]'
any_app="$(pr_green_apply_required_contract example-repo "$empty_status_state" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.verification_pending // false' <<<"$any_app")" false
assert_eq "$(jq -r '.required_checks_missing | length' <<<"$any_app")" 0

# A head move during contract collection must fail closed.
pr_green_test_classic='{"required_status_checks":{"contexts":[],"checks":[]}}'
pr_green_test_rules='[]'
pr_green_test_head_mode=move
pr_green_test_view_count=0
moved_state="$(pr_green_apply_required_contract example-repo "$contract_state" https://github.com/jleechanorg/example-repo/pull/1)"
assert_eq "$(jq -r '.verification_pending' <<<"$moved_state")" true
assert_eq "$(jq -r '.verification_reason' <<<"$moved_state")" pr_head_changed_during_verification

# WorldArchitect's explicit contract remains additive even when both live API
# sources authoritatively report no required checks.
pr_green_test_head_mode=stable
pr_green_test_view_count=0
wa_state="$(pr_green_apply_required_contract worldarchitect.ai "${contract_state/head_repo_name\/jleechanorg\/example-repo/head_repo_name\":\"jleechanorg\/worldarchitect.ai}" https://github.com/jleechanorg/worldarchitect.ai/pull/1)"
assert_eq "$(jq -r '.required_checks | sort | join(",")' <<<"$wa_state")" 'Green Gate,Tests Required Gate'

printf 'outcome accounting tests passed\n'
