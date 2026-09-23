#!/usr/bin/env bash
set -euo pipefail

job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

cat >"$fixture_dir/runs.jsonl" <<'EOF'
{"ts":1000,"discovered":5,"analyzed":4,"actionable":2,"selected":2,"attempted":2,"dispatched":2}
EOF
cat >"$fixture_dir/outcomes.jsonl" <<'EOF'
{"ts":1100,"repo":"worldarchitect.ai","number":1,"url":"https://example.test/1","run_ts":1000,"result":"fixed","verified":true,"detail":"tests passed"}
{"ts":1100,"repo":"worldarchitect.ai","number":2,"url":"https://example.test/2","run_ts":1000,"result":"fixed","verified":false,"detail":"unverified claim"}
{"ts":1100,"repo":"worldarchitect.ai","number":3,"url":"https://example.test/3","run_ts":1000,"result":"blocked","verified":false,"detail":"ambiguous"}
EOF

body="$(PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_NOW=1200 PR_GREEN_REPORT_WINDOW_HOURS=1 "$job_dir/report.sh" stdout)"
rg -q 'Confirmed fixes \(verified\): 1' <<<"$body"
rg -q 'Repair attempts \(dispatch or session reuse\): 2' <<<"$body"
rg -q 'Blocked: 1' <<<"$body"

mock_bin="$fixture_dir/bin"
mkdir -p "$mock_bin"
cat >"$mock_bin/curl" <<'EOF'
#!/usr/bin/env bash
printf 'curl\n' >>"$MOCK_CALLS"
printf '%s\n' '{"ok":true}'
EOF
chmod +x "$mock_bin/curl"
calls="$fixture_dir/calls"
PATH="$mock_bin:$PATH" MOCK_CALLS="$calls" HERMES_SLACK_BOT_TOKEN=test-token \
  PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_NOW=5000 PR_GREEN_REPORT_WINDOW_HOURS=8 \
  "$job_dir/report.sh" slack >/dev/null
[[ "$(wc -l <"$calls")" -eq 1 ]]
PATH="$mock_bin:$PATH" MOCK_CALLS="$calls" HERMES_SLACK_BOT_TOKEN=test-token \
  PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_NOW=5001 PR_GREEN_REPORT_WINDOW_HOURS=8 \
  "$job_dir/report.sh" slack >/dev/null
[[ "$(wc -l <"$calls")" -eq 1 ]]

cat >"$mock_bin/gog" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$MOCK_CALLS"
exit 0
EOF
chmod +x "$mock_bin/gog"
PATH="$mock_bin:$PATH" MOCK_CALLS="$calls" PR_GREEN_METRICS_DIR="$fixture_dir" \
  PR_GREEN_REPORT_NOW=90000 PR_GREEN_REPORT_WINDOW_HOURS=24 "$job_dir/report.sh" email >/dev/null
rg -q '^gmail send ' "$calls"

echo 'report tests passed'
