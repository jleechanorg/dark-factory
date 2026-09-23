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
  printf '%s\n' '{"headRefOid":"head-before","mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","statusCheckRollup":[]}'
  exit 0
fi

printf 'unexpected gh invocation: %s\n' "$*" >&2
exit 1
EOF

cat >"$mock_bin/ao" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${AO_CALLS:?}"
exit 99
EOF
chmod +x "$mock_bin/gh" "$mock_bin/ao"

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

echo 'jleechanorg-pr-green discovery: PASS'
