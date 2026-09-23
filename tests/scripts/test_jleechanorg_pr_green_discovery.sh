#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
JOB="$ROOT/jobs/jleechanorg-pr-green/jleechanorg-pr-green-daily.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
mock_bin="$fixture_dir/bin"
mkdir -p "$mock_bin"

cat >"$mock_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${GH_CALLS:?}"

endpoint=''
for arg in "$@"; do
  case "$arg" in
    /search/issues|/orgs/jleechanorg/repos|/repos/jleechanorg/*/pulls)
      endpoint="$arg"
      ;;
  esac
done

updated="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
item() {
  local repo="$1" number="$2"
  jq -cn --arg repo "$repo" --arg updated "$updated" --argjson number "$number" \
    '{repository_url:("https://api.github.com/repos/jleechanorg/" + $repo),number:$number,title:("pr-" + ($number|tostring)),html_url:("https://github.com/jleechanorg/" + $repo + "/pull/" + ($number|tostring)),updated_at:$updated,draft:false}'
}

if [[ "$1" == api && "$endpoint" == /search/issues ]]; then
  case "${PR_GREEN_DISCOVERY_CASE:?}" in
    multipage)
      page_one="$(jq -cn --arg updated "$updated" '[range(1;61) | {repository_url:"https://api.github.com/repos/jleechanorg/repo-a",number:.,title:("pr-" + (.|tostring)),html_url:("https://github.com/jleechanorg/repo-a/pull/" + (.|tostring)),updated_at:$updated,draft:false}]')"
      page_two="$(jq -cn --arg updated "$updated" '[range(61;102) | {repository_url:"https://api.github.com/repos/jleechanorg/repo-a",number:.,title:("pr-" + (.|tostring)),html_url:("https://github.com/jleechanorg/repo-a/pull/" + (.|tostring)),updated_at:$updated,draft:false}]')"
      jq -cn --argjson page_one "$page_one" --argjson page_two "$page_two" \
        '[{total_count:101,incomplete_results:false,items:$page_one},{total_count:101,incomplete_results:false,items:$page_two}]'
      ;;
    fallback)
      jq -cn '[{total_count:1001,incomplete_results:false,items:[]}]'
      ;;
    missing-gate)
      missing_item="$(item worldarchitect.ai 43)"
      jq -cn --argjson missing_item "$missing_item" \
        '[{total_count:1,incomplete_results:false,items:[$missing_item]}]'
      ;;
    fairness)
      fairness_items="$(jq -cn --arg updated "$updated" '[range(1;14) | {repository_url:"https://api.github.com/repos/jleechanorg/repo-a",number:.,title:("pr-" + (.|tostring)),html_url:("https://github.com/jleechanorg/repo-a/pull/" + (.|tostring)),updated_at:$updated,draft:false}]')"
      jq -cn --argjson fairness_items "$fairness_items" \
        '[{total_count:13,incomplete_results:false,items:$fairness_items}]'
      ;;
    spawn-failure)
      failed_item="$(item repo-a 44)"
      jq -cn --argjson failed_item "$failed_item" \
        '[{total_count:1,incomplete_results:false,items:[$failed_item]}]'
      ;;
    ao)
      ao_item="$(item agent-orchestrator 42)"
      jq -cn --argjson ao_item "$ao_item" \
        '[{total_count:1,incomplete_results:false,items:[$ao_item]}]'
      ;;
    *)
      echo "unknown discovery case: ${PR_GREEN_DISCOVERY_CASE}" >&2
      exit 1
      ;;
  esac
  exit 0
fi

if [[ "$1" == api && "$endpoint" == /orgs/jleechanorg/repos ]]; then
  jq -cn '[ [{name:"repo-a"},{name:"repo-b"}] ]'
  exit 0
fi

if [[ "$1" == api && "$endpoint" == /repos/jleechanorg/repo-a/pulls ]]; then
  pr_one="$(item repo-a 1001)"
  pr_two="$(item repo-a 1002)"
  jq -cn --argjson pr_one "$pr_one" --argjson pr_two "$pr_two" '[[ $pr_one, $pr_two ]]'
  exit 0
fi

if [[ "$1" == api && "$endpoint" == /repos/jleechanorg/repo-b/pulls ]]; then
  pr_three="$(item repo-b 1003)"
  jq -cn --argjson pr_three "$pr_three" '[[ $pr_three ]]'
  exit 0
fi

if [[ "$1 $2" == 'pr view' ]]; then
  if [[ "${PR_GREEN_DISCOVERY_CASE:?}" == missing-gate ]]; then
    printf '%s\n' '{"headRefOid":"head-clean","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[{"name":"unit","status":"COMPLETED","conclusion":"SUCCESS"}]}'
    exit 0
  fi
  printf '%s\n' '{"headRefOid":"head-before","mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","statusCheckRollup":[]}'
  exit 0
fi

printf 'unexpected gh invocation: %s\n' "$*" >&2
exit 1
EOF

cat >"$mock_bin/ao" <<'EOF'
#!/usr/bin/env bash
if [[ "${PR_GREEN_BUSY_FAIRNESS:-0}" == 1 ]]; then
  if [[ "$1" == spawn ]]; then
    if [[ "${PR_GREEN_SPAWN_FAILURE:-0}" == 1 ]]; then
      [[ "${PR_GREEN_SPAWN_FAILURE_OUTPUT:-0}" == 1 ]] && printf '%s\n' 'spawn failed'
      exit 23
    fi
    sleep 0.2
    exit 0
  fi
  case "$1 $2" in
    "session ls")
      printf '%s\n' '{"data":[{"id":"busy-1","displayName":"pr-1","isTerminated":false,"status":"pr_open"}]}'
      exit 0
      ;;
    "project get")
      printf '%s\n' '{"project":{"config":{"env":{"CODEX_HOME":"'"${CODEX_HOME:-}"'"}}}}'
      exit 0
      ;;
    "project set-config")
      printf '%s\n' '{"status":"ok"}'
      exit 0
      ;;
    *)
      exit 0
      ;;
  esac
fi
printf '%s\n' "$*" >>"${AO_CALLS:?}"
exit 99
EOF
chmod +x "$mock_bin/gh" "$mock_bin/ao"

cat >"$mock_bin/sqlite3" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' 'runtime-1'
EOF
cat >"$mock_bin/tmux" <<'EOF'
#!/usr/bin/env bash
if [[ "$1" == has-session ]]; then
  exit 0
fi
if [[ "$1" == capture-pane ]]; then
  printf '%s\n' 'Working (4m 12s)'
  exit 0
fi
exit 1
EOF
chmod +x "$mock_bin/sqlite3" "$mock_bin/tmux"

run_case() {
  local name="$1" expected_discovered="$2" dry_run="${3:-1}" max_prs="${4:-0}"
  local metrics="$fixture_dir/metrics-$name" calls="$fixture_dir/ao-$name.log"
  mkdir -p "$metrics"
  : >"$calls"
  set +e
  PATH="$mock_bin:$PATH" \
    HOME="$fixture_dir/home-$name" \
    PR_GREEN_DISCOVERY_CASE="$name" \
    PR_GREEN_METRICS_DIR="$metrics" \
    PR_GREEN_MAX_PRS="$max_prs" \
    PR_GREEN_DRY_RUN="$dry_run" \
    AO_CALLS="$calls" \
    GH_CALLS="$fixture_dir/gh-$name.log" \
    bash "$JOB" >"$fixture_dir/stdout-$name" 2>"$fixture_dir/stderr-$name"
  local rc=$?
  set -e
  [[ "$rc" == 0 ]] || {
    cat "$fixture_dir/stderr-$name" >&2
    return 1
  }
  run_json="$(jq -s 'last' "$metrics/runs.jsonl")"
  [[ "$(jq -r '.discovered' <<<"$run_json")" == "$expected_discovered" ]] || {
    echo "FAIL: $name discovered $(jq -r '.discovered' <<<"$run_json"), expected $expected_discovered" >&2
    return 1
  }
  printf '%s\n' "$metrics"
}

multi_metrics="$(run_case multipage 101)"
[[ "$(find "$multi_metrics" -name 'discovery-*.tsv' -exec awk 'NF {n++} END {print n+0}' {} \;)" == 101 ]] || {
  echo 'FAIL: multi-page discovery did not persist all records' >&2
  exit 1
}
multi_audit="$(find "$multi_metrics" -name 'discovery-*.json' -print -quit)"
[[ "$(jq -r '.source' "$multi_audit")" == search && "$(jq -r '.total_count' "$multi_audit")" == 101 && "$(jq -r '.incomplete_results' "$multi_audit")" == false ]] || {
  echo 'FAIL: search discovery audit did not retain cutoff/page metadata' >&2
  exit 1
}

fallback_metrics="$(run_case fallback 3)"
grep -Fq '/orgs/jleechanorg/repos' "$fixture_dir/gh-fallback.log" || {
  echo 'FAIL: >1000 search result did not use repository fallback' >&2
  exit 1
}
fallback_audit="$(find "$fallback_metrics" -name 'discovery-*.json' -print -quit)"
[[ "$(jq -r '.source' "$fallback_audit")" == repo_fallback && "$(jq -r '.total_count' "$fallback_audit")" == 1001 ]] || {
  echo 'FAIL: fallback discovery audit did not retain over-cap search metadata' >&2
  exit 1
}

# agent-orchestrator remains in discovery, but it is never an AO mutation target.
ao_metrics="$(run_case ao 1 0 1)"
[[ ! -s "$fixture_dir/ao-ao.log" ]] || {
  echo 'FAIL: agent-orchestrator was sent to AO' >&2
  exit 1
}
grep -Fq 'authorization_excluded' "$ao_metrics/outcomes.jsonl" || {
  echo 'FAIL: excluded AO repository was not durably accounted for' >&2
  exit 1
}

# Missing required gates on an otherwise clean PR are pending verification,
# not a repair admission, and must not contact AO.
missing_metrics="$(run_case missing-gate 1 0 1)"
[[ ! -s "$fixture_dir/ao-missing-gate.log" ]] || {
  echo 'FAIL: clean missing-gate PR contacted AO' >&2
  exit 1
}
missing_run="$(jq -s 'last' "$missing_metrics/runs.jsonl")"
[[ "$(jq -r '.actionable' <<<"$missing_run")" == 0 && "$(jq -r '.selected' <<<"$missing_run")" == 0 ]] || {
  echo 'FAIL: clean missing-gate PR was admitted as actionable' >&2
  exit 1
}

# A bounded cap must rotate across runs. PR #1 is visibly busy, but PR #13
# must still be selected on the second run instead of being starved forever.
fair_metrics="$fixture_dir/metrics-fairness"
fair_calls="$fixture_dir/ao-fairness.log"
fair_codex="$fixture_dir/codex-fairness"
mkdir -p "$fair_metrics" "$fair_codex"
printf '%s\n' '{"tokens":{}}' >"$fair_codex/auth.json"
: >"$fair_calls"
for iteration in 1 2; do
  PATH="$mock_bin:$PATH" \
    HOME="$fixture_dir/home-fairness" \
    CODEX_HOME="$fair_codex" \
    PR_GREEN_DISCOVERY_CASE=fairness \
    PR_GREEN_BUSY_FAIRNESS=1 \
    PR_GREEN_AO_DB_PATH="$fixture_dir/ao.db" \
    PR_GREEN_METRICS_DIR="$fair_metrics" \
    PR_GREEN_MAX_PRS=12 \
    PR_GREEN_DRY_RUN=0 \
    PR_GREEN_SPAWN_PROBE_SECONDS=0.05 \
    AO_CALLS="$fair_calls" \
    GH_CALLS="$fixture_dir/gh-fairness-$iteration.log" \
    bash "$JOB" >"$fixture_dir/fairness-$iteration.out" 2>"$fixture_dir/fairness-$iteration.err"
  if [[ "$iteration" == 1 ]]; then
    cp "$fair_metrics/selection-cursor" "$fixture_dir/cursor-before"
    sleep 0.4
  fi
done
grep -Fq 'repo-a#13' "$fixture_dir/fairness-2.out" || {
  cat "$fixture_dir/fairness-2.out" >&2
  cat "$fixture_dir/fairness-2.err" >&2
  cat "$fair_metrics/selection-cursor" >&2 || true
  cat "$fixture_dir/cursor-before" >&2 || true
  cat "$fair_metrics/runs.jsonl" >&2 || true
  echo 'FAIL: PR #13 remained starved after the second bounded run' >&2
  exit 1
}
second_fair_run="$(jq -s 'last' "$fair_metrics/runs.jsonl")"
[[ "$(jq -r '.selected' <<<"$second_fair_run")" -gt 0 ]] || {
  echo 'FAIL: fairness second run did not select any rotated candidate' >&2
  exit 1
}

# An exited, empty-output spawn is a failed attempt, not a dispatch. It must
# still emit durable accounting so attempted and outcome records reconcile.
failure_metrics="$fixture_dir/metrics-spawn-failure"
failure_codex="$fixture_dir/codex-spawn-failure"
mkdir -p "$failure_metrics" "$failure_codex"
printf '%s\n' '{"tokens":{}}' >"$failure_codex/auth.json"
PATH="$mock_bin:$PATH" \
  HOME="$fixture_dir/home-spawn-failure" \
  CODEX_HOME="$failure_codex" \
  PR_GREEN_DISCOVERY_CASE=spawn-failure \
  PR_GREEN_BUSY_FAIRNESS=1 \
  PR_GREEN_SPAWN_FAILURE=1 \
  PR_GREEN_AO_DB_PATH="$fixture_dir/ao-failure.db" \
  PR_GREEN_METRICS_DIR="$failure_metrics" \
  PR_GREEN_MAX_PRS=1 \
  PR_GREEN_DRY_RUN=0 \
  PR_GREEN_SPAWN_PROBE_SECONDS=0.05 \
  AO_CALLS="$fixture_dir/ao-spawn-failure.log" \
  GH_CALLS="$fixture_dir/gh-spawn-failure.log" \
  bash "$JOB" >/dev/null
failure_run="$(jq -s 'last' "$failure_metrics/runs.jsonl")"
[[ "$(jq -r '.attempted' <<<"$failure_run")" == 1 && "$(jq -r '.dispatched' <<<"$failure_run")" == 0 ]] || {
  echo 'FAIL: failed spawn was counted as dispatched' >&2
  exit 1
}
[[ "$(jq -sr 'last.session_action' "$failure_metrics/outcomes.jsonl")" == spawn_failed ]] || {
  echo 'FAIL: failed spawn did not emit durable spawn_failed outcome' >&2
  exit 1
}

retry_metrics="$fixture_dir/metrics-spawn-retry-failure"
mkdir -p "$retry_metrics"
PATH="$mock_bin:$PATH" \
  HOME="$fixture_dir/home-spawn-retry-failure" \
  CODEX_HOME="$failure_codex" \
  PR_GREEN_DISCOVERY_CASE=spawn-failure \
  PR_GREEN_BUSY_FAIRNESS=1 \
  PR_GREEN_SPAWN_FAILURE=1 \
  PR_GREEN_SPAWN_FAILURE_OUTPUT=1 \
  PR_GREEN_AO_DB_PATH="$fixture_dir/ao-retry.db" \
  PR_GREEN_METRICS_DIR="$retry_metrics" \
  PR_GREEN_MAX_PRS=1 \
  PR_GREEN_DRY_RUN=0 \
  PR_GREEN_SPAWN_PROBE_SECONDS=0.05 \
  AO_CALLS="$fixture_dir/ao-spawn-retry-failure.log" \
  GH_CALLS="$fixture_dir/gh-spawn-retry-failure.log" \
  bash "$JOB" >/dev/null
[[ "$(jq -sr 'last.session_action' "$retry_metrics/outcomes.jsonl")" == spawn_retry_failed ]] || {
  echo 'FAIL: failed registration retry did not emit durable retry outcome' >&2
  exit 1
}
retry_run="$(jq -s 'last' "$retry_metrics/runs.jsonl")"
[[ "$(jq -r '.dispatched' <<<"$retry_run")" == 0 ]] || {
  echo 'FAIL: failed registration retry was counted as dispatched' >&2
  exit 1
}

echo 'jleechanorg-pr-green discovery: PASS'
