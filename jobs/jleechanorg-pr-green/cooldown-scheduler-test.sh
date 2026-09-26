#!/usr/bin/env bash
set -euo pipefail

# Exercise the scheduler boundary: an unchanged durable blocker is still
# discovered/analyzed but must not invoke AO at all.
job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
mock_bin="$fixture_dir/bin"
metrics_dir="$fixture_dir/metrics"
mkdir -p "$mock_bin" "$metrics_dir"

# Capacity deferral is an admission result, not evidence that inference ran.
# It must not become the same-head cooldown anchor for the next sweep.
# shellcheck disable=SC1091 # dynamic source is anchored to this test's directory
source "$job_dir/outcome-accounting.sh"
capacity_outcomes="$fixture_dir/capacity-outcomes.jsonl"
printf '%s\n' '{"ts":999,"repo":"worldarchitect.ai","number":9942,"head_after":"same-head","blocker_after":{"conflicting":true,"failed_checks":[]},"classification":"no_change","session_action":"admission_cap_deferred"}' >"$capacity_outcomes"
capacity_state='{"head_sha":"same-head","conflicting":true,"failed_checks":[]}'
if pr_green_same_head_cooldown_applies "$capacity_outcomes" worldarchitect.ai 9942 "$capacity_state" 1000 28800; then
  echo 'capacity deferral unexpectedly consumed inference cooldown' >&2
  exit 1
fi

now="$(date +%s)"
cat >"$metrics_dir/outcomes.jsonl" <<EOF
{"ts":$((now - 1)),"repo":"worldarchitect.ai","number":9941,"head_after":"same-head","blocker_after":{"conflicting":true,"failed_checks":[]},"classification":"no_change","session_action":"reused"}
EOF

cat >"$mock_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "api" ]]; then
  updated="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  jq -cn --arg updated "$updated" '[{total_count:1,incomplete_results:false,items:[{repository_url:"https://api.github.com/repos/jleechanorg/worldarchitect.ai",number:9941,title:"conflict",html_url:"https://github.com/jleechanorg/worldarchitect.ai/pull/9941",updated_at:$updated,draft:false}]}]'
  exit 0
fi
if [[ "$1 $2" == "pr view" ]]; then
  printf '%s\n' '{"headRefOid":"same-head","mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","statusCheckRollup":[]}'
  exit 0
fi
printf 'unexpected gh call: %s\n' "$*" >&2
exit 1
EOF
cat >"$mock_bin/ao" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$AO_CALLS"
exit 99
EOF
chmod +x "$mock_bin/gh" "$mock_bin/ao"

ao_calls="$fixture_dir/ao-calls"
PATH="$mock_bin:$PATH" AO_CALLS="$ao_calls" PR_GREEN_METRICS_DIR="$metrics_dir" \
  PR_GREEN_MAX_PRS=1 PR_GREEN_SPAWN_PROBE_SECONDS=0 "$job_dir/jleechanorg-pr-green-daily.sh" >/dev/null

[[ ! -s "$ao_calls" ]] || { echo 'cooldown unexpectedly contacted AO' >&2; exit 1; }
run="$(jq -sc 'last' "$metrics_dir/runs.jsonl")"
[[ "$(jq -r '.analyzed' <<<"$run")" == "1" ]]
[[ "$(jq -r '.actionable' <<<"$run")" == "1" ]]
[[ "$(jq -r '.selected' <<<"$run")" == "0" ]]
[[ "$(jq -r '.attempted' <<<"$run")" == "0" ]]
[[ "$(jq -r '.cooldown_deferred' <<<"$run")" == "1" ]]
[[ "$(jq -sr 'last.session_action' "$metrics_dir/outcomes.jsonl")" == "cooldown_deferred" ]]

echo 'cooldown scheduler tests passed'
