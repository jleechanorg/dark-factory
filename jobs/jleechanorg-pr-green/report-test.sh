#!/usr/bin/env bash
set -euo pipefail

job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

cat >"$fixture_dir/runs.jsonl" <<'EOF'
{"ts":1000,"discovered":5,"analyzed":4,"actionable":2,"selected":2,"attempted":2,"dispatched":2,"busy_deferred":1,"cooldown_deferred":3}
EOF
cat >"$fixture_dir/outcomes.jsonl" <<'EOF'
{"ts":1100,"repo":"worldarchitect.ai","number":1,"url":"https://example.test/1","run_ts":1000,"head_before":"old-head","head_after":"new-head","session_action":"dispatched","result":"fixed","verified":true,"detail":"tests passed"}
{"ts":1150,"repo":"worldarchitect.ai","number":1,"url":"https://example.test/1","run_ts":1050,"head_before":"old-head","head_after":"new-head","session_action":"reconciled","result":"fixed","verified":true,"detail":"reconciled duplicate"}
{"ts":1100,"repo":"worldarchitect.ai","number":2,"url":"https://example.test/2","run_ts":1000,"result":"fixed","verified":false,"detail":"unverified claim"}
{"ts":1100,"repo":"worldarchitect.ai","number":3,"url":"https://example.test/3","run_ts":1000,"result":"blocked","verified":false,"detail":"ambiguous"}
EOF

body="$(PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_NOW=1200 PR_GREEN_REPORT_WINDOW_HOURS=1 "$job_dir/report.sh" stdout)"
rg -q 'Verified new-head green outcomes \(job-attributed, unique PR/head\): 1' <<<"$body"
rg -q 'Observed green outcomes from reconciliation \(attribution not independently verified, unique PR/head\): 1' <<<"$body"
rg -q 'Repair attempts \(dispatch or session reuse\): 2' <<<"$body"
rg -q 'Busy sessions deferred \(no prompt queued\): 1' <<<"$body"
rg -q 'Unchanged blockers deferred by cooldown \(no inference\): 3' <<<"$body"
rg -q 'Blocked: 1' <<<"$body"

mock_bin="$fixture_dir/bin"
mkdir -p "$mock_bin"
cat >"$mock_bin/curl" <<'EOF'
#!/usr/bin/env bash
printf 'curl %s\n' "$*" >>"$MOCK_CALLS"
if [[ "$1" == --config && "$2" == - ]]; then
  config="$(cat)"
  [[ "$config" == *'test-token'* || "$config" == *'test-smtp-pass'* ]] || exit 1
fi
if compgen -G "$PR_GREEN_METRICS_DIR/smtp-config.*" >/dev/null; then
  echo 'SMTP credential file exists during delivery' >&2
  exit 1
fi
sleep "${MOCK_CURL_DELAY:-0}"
printf '%s\n' '{"ok":true}'
EOF
chmod +x "$mock_bin/curl"
calls="$fixture_dir/calls"
PATH="$mock_bin:$PATH" MOCK_CALLS="$calls" HERMES_SLACK_BOT_TOKEN=test-token \
  PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_NOW=5000 PR_GREEN_REPORT_WINDOW_HOURS=8 \
  "$job_dir/report.sh" slack >/dev/null
[[ "$(rg -c '^curl ' "$calls")" -eq 1 ]]
if rg -q 'test-token' "$calls"; then
  echo 'Slack token leaked into curl arguments' >&2
  exit 1
fi
PATH="$mock_bin:$PATH" MOCK_CALLS="$calls" HERMES_SLACK_BOT_TOKEN=test-token \
  PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_NOW=5001 PR_GREEN_REPORT_WINDOW_HOURS=8 \
  "$job_dir/report.sh" slack >/dev/null
[[ "$(rg -c '^curl ' "$calls")" -eq 1 ]]

cat >"$mock_bin/gog" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$MOCK_CALLS"
exit 1
EOF
chmod +x "$mock_bin/gog"
PATH="$mock_bin:$PATH" MOCK_CALLS="$calls" EMAIL_USER=test-smtp-user EMAIL_PASS=test-smtp-pass PR_GREEN_METRICS_DIR="$fixture_dir" \
  PR_GREEN_REPORT_NOW=90000 PR_GREEN_REPORT_WINDOW_HOURS=24 "$job_dir/report.sh" email >/dev/null
rg -q '^gmail send ' "$calls"
rg -q '^curl --config ' "$calls"
if rg -q 'test-smtp-(user|pass)' "$calls"; then
  echo 'SMTP credentials leaked into curl arguments' >&2
  exit 1
fi
if compgen -G "$fixture_dir/smtp-config.*" >/dev/null; then
  echo 'SMTP credential file persisted' >&2
  exit 1
fi
state="$(jq -r '.email.last_sent_at' "$fixture_dir/report-state.json")"
[[ "$state" == "90000" ]]

if env -u PR_GREEN_SMTP_USER -u PR_GREEN_SMTP_PASS -u EMAIL_USER -u EMAIL_PASS \
  PATH="$mock_bin:$PATH" MOCK_CALLS="$calls" PR_GREEN_METRICS_DIR="$fixture_dir" \
  PR_GREEN_REPORT_NOW=180000 PR_GREEN_REPORT_WINDOW_HOURS=24 "$job_dir/report.sh" email >/dev/null 2>&1; then
  echo 'email unexpectedly succeeded without SMTP credentials' >&2
  exit 1
fi
[[ "$(jq -r '.email.last_sent_at' "$fixture_dir/report-state.json")" == "90000" ]]

# A manual evening delivery must not suppress the next calendar day's email.
TZ=UTC PATH="$mock_bin:$PATH" MOCK_CALLS="$calls" EMAIL_USER=test-smtp-user EMAIL_PASS=test-smtp-pass \
  PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_NOW=172801 PR_GREEN_REPORT_WINDOW_HOURS=24 \
  "$job_dir/report.sh" email >/dev/null
[[ "$(jq -r '.email.last_sent_at' "$fixture_dir/report-state.json")" == "172801" ]]

if env -u HERMES_SLACK_BOT_TOKEN -u SLACK_BOT_TOKEN \
  PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_NOW=180000 \
  "$job_dir/report.sh" slack >/dev/null 2>&1; then
  echo 'Slack unexpectedly succeeded without credentials' >&2
  exit 1
fi

# Concurrent modes must retain both cadence entries; duplicate Slack calls
# must share the same check/delivery/update lock.
race_state="$fixture_dir/race-state.json"
race_calls="$fixture_dir/race-calls"
for mode in slack email slack; do
  env PATH="$mock_bin:$PATH" MOCK_CALLS="$race_calls" MOCK_CURL_DELAY=0.1 \
    HERMES_SLACK_BOT_TOKEN=test-token EMAIL_USER=test-smtp-user EMAIL_PASS=test-smtp-pass \
    PR_GREEN_METRICS_DIR="$fixture_dir" PR_GREEN_REPORT_STATE_FILE="$race_state" \
    PR_GREEN_REPORT_NOW=200000 "$job_dir/report.sh" "$mode" >/dev/null &
  pids+=("$!")
done
for pid in "${pids[@]}"; do wait "$pid"; done
jq -e '.slack.last_sent_at == 200000 and .email.last_sent_at == 200000' "$race_state" >/dev/null
[[ "$(rg -c '^curl ' "$race_calls")" -eq 2 ]]

echo 'report tests passed'
