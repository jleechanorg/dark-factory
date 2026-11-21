#!/usr/bin/env bash
# Regression test for the report.sh lane-freshness guard.
#
# When the sweep lane is stopped, a zero-run window must render a LANE DOWN
# outage notice instead of a table of zeros that reads as "nothing to do".
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPORT_SH="$SCRIPT_DIR/report.sh"
[[ -f "$REPORT_SH" ]] || { echo "FAIL: $REPORT_SH not found" >&2; exit 1; }

tmp="$(mktemp -d "${TMPDIR:-/tmp}/pr-green-freshness.XXXXXX")"
trap 'rm -rf -- "$tmp"' EXIT

now=1792700000
window=4
failures=0

seed_state() {  # $1=dir $2=run_ts
  local dir="$1" run_ts="$2"
  mkdir -p "$dir"
  printf '{"ts":%s,"discovered":1,"analyzed":1,"actionable":0,"selected":0,"attempted":0}\n' "$run_ts" >"$dir/runs.jsonl"
  : >"$dir/outcomes.jsonl"
  : >"$dir/push-receipts.jsonl"
  printf 'worldarchitect.ai\t10708\n' >"$dir/discovery-${run_ts}.tsv"
}

run_report() {  # $1=dir
  PR_GREEN_METRICS_DIR="$1" \
  PR_GREEN_REPORT_STATE_FILE="$1/report-state.json" \
  PR_GREEN_REPORT_NOW="$now" \
  PR_GREEN_REPORT_WINDOW_HOURS="$window" \
  bash "$REPORT_SH" stdout 2>&1
}

check() {  # $1=label $2=condition-result $3=message
  if [[ "$2" == "0" ]]; then
    echo "ok   - $1"
  else
    echo "FAIL - $1: $3" >&2
    failures=$((failures + 1))
  fi
}

# --- stale lane: last run is 16h old, outside the 4h window ----------------
stale_dir="$tmp/stale"
seed_state "$stale_dir" "$((now - 16 * 3600))"
stale_out="$(run_report "$stale_dir")"

grep -q 'LANE DOWN' <<<"$stale_out"; check "stale reports LANE DOWN" $? "$stale_out"
grep -q 'Coverage: runs 0' <<<"$stale_out"; [[ $? -ne 0 ]]; check "stale omits zero-coverage table" $? "$stale_out"
grep -q 'The sweep lane is stopped or disabled' <<<"$stale_out"; check "stale names the outage" $? "$stale_out"
grep -q 'Policy: only explicit red CI or merge conflicts' <<<"$stale_out"; check "stale keeps policy footer" $? "$stale_out"

# --- fresh lane: a run inside the window keeps the normal report ------------
fresh_dir="$tmp/fresh"
seed_state "$fresh_dir" "$((now - 600))"
fresh_out="$(run_report "$fresh_dir")"

grep -q 'LANE DOWN' <<<"$fresh_out"; [[ $? -ne 0 ]]; check "fresh omits LANE DOWN" $? "$fresh_out"
grep -q 'Coverage: runs 1' <<<"$fresh_out"; check "fresh keeps normal coverage line" $? "$fresh_out"
grep -q 'PRs analyzed:' <<<"$fresh_out"; check "fresh keeps analyzed line" $? "$fresh_out"

if (( failures > 0 )); then
  echo "FAILED: $failures assertion(s)" >&2
  exit 1
fi
echo "PASS: lane-freshness guard (2 scenarios)"
