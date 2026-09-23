#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
JOB="$ROOT/jobs/jleechanorg-pr-green/jleechanorg-pr-green-daily.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
mock_bin="$fixture_dir/bin"
metrics_dir="$fixture_dir/metrics"
state_file="$fixture_dir/state.json"
mkdir -p "$mock_bin" "$metrics_dir/pr-state"

cat >"$mock_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "search" && "$2" == "prs" ]]; then
  updated="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  jq -cn --arg updated "$updated" '[{repository:{name:"worldarchitect.ai"},number:9941,title:"reconciled",url:"https://github.com/jleechanorg/worldarchitect.ai/pull/9941",updatedAt:$updated,isDraft:false}]'
elif [[ "$1" == "pr" && "$2" == "view" ]]; then
  if [[ -n "${PR_GREEN_VIEW_STATE_FILE:-}" ]]; then
    cat "$PR_GREEN_VIEW_STATE_FILE"
  else
    printf '%s\n' '{"headRefOid":"green-head","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[]}'
  fi
else
  printf 'unexpected gh invocation: %s\n' "$*" >&2
  exit 1
fi
EOF

cat >"$mock_bin/ao" <<'EOF'
#!/usr/bin/env bash
printf 'AO must not be contacted while reconciling a green snapshot: %s\n' "$*" >&2
exit 1
EOF
chmod +x "$mock_bin/gh" "$mock_bin/ao"

cat >"$metrics_dir/pr-state/worldarchitect.ai-9941.json" <<'EOF'
{"head_sha":"blocked-head","conflicting":true,"failed_checks":[],"pending_checks":false}
EOF

PATH="$mock_bin:$PATH" \
  HOME="$fixture_dir/home" \
  PR_GREEN_METRICS_DIR="$metrics_dir" \
  bash "$JOB" >/dev/null

[[ ! -e "$metrics_dir/pr-state/worldarchitect.ai-9941.json" ]] || {
  echo 'FAIL: reconciled green snapshot was not retired' >&2
  exit 1
}

# A second scan must not re-count the already reconciled fixed head after the
# snapshot has been retired, even though the PR remains in the discovery set.
PATH="$mock_bin:$PATH" \
  HOME="$fixture_dir/home" \
  PR_GREEN_METRICS_DIR="$metrics_dir" \
  bash "$JOB" >/dev/null
[[ "$(jq -s '[.[] | select(.result == "fixed" and .verified == true)] | length' "$metrics_dir/outcomes.jsonl")" == 1 ]] || {
  echo 'FAIL: repeated green snapshot scan counted a duplicate confirmed fix' >&2
  exit 1
}

# A pushed replacement head can be observed while its checks are still
# pending. Keep the original blocker snapshot through that intermediate scan;
# only the later green read may retire it and emit one confirmed fix.
fixed_before="$(jq -s '[.[] | select(.result == "fixed" and .verified == true)] | length' "$metrics_dir/outcomes.jsonl")"
cat >"$state_file" <<'EOF'
{"headRefOid":"replacement-head","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{"name":"unit","status":"IN_PROGRESS"}]}
EOF
printf '%s\n' '{"head_sha":"blocked-head","conflicting":true,"failed_checks":[],"pending_checks":false}' >"$metrics_dir/pr-state/worldarchitect.ai-9941.json"
PATH="$mock_bin:$PATH" \
  HOME="$fixture_dir/home" \
  PR_GREEN_METRICS_DIR="$metrics_dir" \
  PR_GREEN_VIEW_STATE_FILE="$state_file" \
  bash "$JOB" >/dev/null
[[ -e "$metrics_dir/pr-state/worldarchitect.ai-9941.json" ]] || {
  echo 'FAIL: pending replacement head retired the blocker snapshot' >&2
  exit 1
}

cat >"$state_file" <<'EOF'
{"headRefOid":"replacement-head","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{"name":"unit","status":"COMPLETED","conclusion":"SUCCESS"}]}
EOF
PATH="$mock_bin:$PATH" \
  HOME="$fixture_dir/home" \
  PR_GREEN_METRICS_DIR="$metrics_dir" \
  PR_GREEN_VIEW_STATE_FILE="$state_file" \
  bash "$JOB" >/dev/null
[[ ! -e "$metrics_dir/pr-state/worldarchitect.ai-9941.json" ]] || {
  echo 'FAIL: green replacement head did not retire the original snapshot' >&2
  exit 1
}
fixed_after="$(jq -s '[.[] | select(.result == "fixed" and .verified == true)] | length' "$metrics_dir/outcomes.jsonl")"
[[ "$fixed_after" == "$((fixed_before + 1))" ]] || {
  echo "FAIL: pending-to-green lifecycle emitted $((fixed_after - fixed_before)) fixes" >&2
  exit 1
}

echo 'jleechanorg-pr-green snapshot lifecycle: PASS'
