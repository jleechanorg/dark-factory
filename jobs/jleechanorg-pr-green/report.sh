#!/usr/bin/env bash
set -euo pipefail

# Build and deliver a bounded repair report from durable scheduler and PR-outcome
# records.  A dispatch is never presented as a fix: only an outcome record with
# result=fixed and verified=true is a confirmed fix.

MODE="${1:-slack}"
case "$MODE" in
  slack|email|stdout) ;;
  *) echo "usage: $0 [slack|email|stdout]" >&2; exit 64 ;;
esac

METRICS_DIR="${PR_GREEN_METRICS_DIR:-$HOME/.local/state/jleechanorg-pr-green}"
STATE_FILE="${PR_GREEN_REPORT_STATE_FILE:-$METRICS_DIR/report-state.json}"
NOW="${PR_GREEN_REPORT_NOW:-$(date +%s)}"
WINDOW_HOURS="${PR_GREEN_REPORT_WINDOW_HOURS:-8}"
WINDOW_SECONDS=$((WINDOW_HOURS * 3600))
SINCE=$((NOW - WINDOW_SECONDS))
RUNS_FILE="$METRICS_DIR/runs.jsonl"
OUTCOMES_FILE="$METRICS_DIR/outcomes.jsonl"

mkdir -p "$METRICS_DIR"
# Delivery and cadence updates share one lock, including Slack/email overlap.
if [[ "$MODE" != "stdout" ]]; then
  exec 9>"${STATE_FILE}.lock"
  flock -x 9
fi
[[ -f "$RUNS_FILE" ]] || : >"$RUNS_FILE"
[[ -f "$OUTCOMES_FILE" ]] || : >"$OUTCOMES_FILE"

# Index discovery snapshots without treating report generation as a scheduler
# run. A snapshot is trusted only when its row count agrees with both run totals.
discovery_records_file="$(mktemp "${TMPDIR:-/tmp}/pr-green-report-discovery.XXXXXX")"
trap 'rm -f -- "$discovery_records_file"' EXIT
shopt -s nullglob
for discovery_file in "$METRICS_DIR"/discovery-*.tsv; do
  run_ts="${discovery_file##*/discovery-}"
  run_ts="${run_ts%.tsv}"
  [[ "$run_ts" =~ ^[0-9]+$ ]] || continue
  jq -Rn --argjson run_ts "$run_ts" '
    [inputs | select(length > 0) | split("\t") | select(length >= 2)
      | {repo: .[0], number: .[1]}]
    | {run_ts: $run_ts, rows: ., row_count: length}
  ' <"$discovery_file" >>"$discovery_records_file"
done
shopt -u nullglob

summary="$(jq -s --slurpfile outcomes "$OUTCOMES_FILE" \
  --slurpfile discoveries "$discovery_records_file" --argjson since "$SINCE" '
  def recent: map(select((.ts // 0) >= $since));
  def valid_push_receipt:
    (.push_receipt? // null) as $receipt
    | ($receipt | type) == "object"
      and ($receipt.verified == true)
      and ($receipt.push_exit_code == 0)
      and (($receipt.before_sha // "") | tostring | length) > 0
      and (($receipt.after_sha // "") | tostring | length) > 0
      and ($receipt.before_sha != $receipt.after_sha)
      and (($receipt.commit_url // "") | tostring | length) > 0
      and (($receipt.repo // "") | tostring | length) > 0
      and (($receipt.session_id // $receipt.session // "") | tostring | length) > 0;
  (recent) as $runs |
  ($outcomes | recent) as $outcomes |
  ($discoveries
    | map(. as $discovery
      | ($runs | map(select((.ts // 0) == $discovery.run_ts)) | .[0]) as $run
      | select($run != null)
      | {
          run_ts: $discovery.run_ts,
          rows: $discovery.rows,
          row_count: $discovery.row_count,
          complete: ($run != null
            and ($run.discovered // -1) == $discovery.row_count
            and ($run.analyzed // -1) == $discovery.row_count)
        })) as $discovery_runs |
  ([ $discovery_runs[] | select(.complete) | .rows[] ]
    | unique_by([.repo, .number])) as $unique_discovered |
  ((($runs | length) > 0)
    and (($discovery_runs | length) == ($runs | length))
    and all($discovery_runs[]; .complete)) as $discovery_complete |
  ($outcomes
    | map(select(valid_push_receipt))
    | unique_by([.repo, .number])) as $pushes |
  {
    runs: ($runs | length),
    discovered: ($runs | map(.discovered // 0) | add // 0),
    analyzed: ($runs | map(.analyzed // 0) | add // 0),
    actionable: ($runs | map(.actionable // 0) | add // 0),
    selected: ($runs | map(.selected // 0) | add // 0),
    attempts: ($runs | map(.attempted // 0) | add // 0),
    busy_deferred: ($runs | map(.busy_deferred // 0) | add // 0),
    cooldown_deferred: ($runs | map(.cooldown_deferred // 0) | add // 0),
    analyzed_unique: ($unique_discovered | length),
    analyzed_coverage_complete: $discovery_complete,
    covered_runs: ($discovery_runs | map(select(.complete)) | length),
    pushes: $pushes,
    push_receipts_available: (any($outcomes[]; valid_push_receipt)),
    green_new_head_outcomes: ($outcomes
      | map(select(.result == "fixed" and (.verified == true)
        and ((.head_before // "") != (.head_after // ""))))
      | unique_by([.repo, .number, .head_after])
      | length),
    recovery_blocked: ($outcomes
      | map(select(.session_action == "recovery_blocked"))
      | unique_by([.repo, .number, .head_after])
      | length),
    delivery_unconfirmed: ($outcomes
      | map(select(.session_action == "delivery_unconfirmed"))
      | unique_by([.repo, .number, .head_after])
      | length),
    native_ack_observed: ($outcomes
      | map(select(.native_ack_status == "observed"))
      | length),
    native_ack_missing: ($outcomes
      | map(select(.native_ack_status == "missing"))
      | length),
    native_ack_untracked: ($outcomes
      | map(select((.native_ack_status // "untracked") == "untracked"))
      | length),
    blockers: ($outcomes | map(select(.result == "blocked")) | length),
    unchanged: ($outcomes | map(select(.result == "no_change")) | length),
    in_progress: ($outcomes | map(select(.result == "in_progress")) | length),
    dispatch_failures: ($outcomes | map(select(.result == "dispatch_failed")) | length)
  }
' "$RUNS_FILE")"

analyzed_label="$(jq -r 'if .analyzed_coverage_complete then (.analyzed_unique | tostring) elif .covered_runs > 0 then ("at least " + (.analyzed_unique | tostring) + " unique (" + (.covered_runs | tostring) + "/" + (.runs | tostring) + " runs covered)") else "unknown (coverage incomplete)" end' <<<"$summary")"
pushed_label="$(jq -r 'if .push_receipts_available then (.pushes | length | tostring) else "unknown (push receipts unavailable)" end' <<<"$summary")"
push_lines="$(jq -r '
  .pushes[]?
  | "- " + (.repo // "unknown-repo") + "#" + ((.number // "?") | tostring)
    + ": " + (.url // "PR URL unavailable")
    + " — evidence "
    + (if (.push_receipt.commit_url? // "") != "" then .push_receipt.commit_url
       elif (.push_receipt.url? // "") != "" then .push_receipt.url
       elif (.push_receipt.commit? // "") != "" then .push_receipt.commit
       else "receipt recorded; evidence URL unavailable" end)
' <<<"$summary")"
[[ -n "$push_lines" ]] || push_lines='(none; no push receipt is available)'

body="PR green repair report (last ${WINDOW_HOURS}h)

PRs analyzed: ${analyzed_label}
PRs with successful remote commits: ${pushed_label}
Verified green PR/heads (push attribution not established): $(jq -r '.green_new_head_outcomes' <<<"$summary")

Verified pushed PRs (receipt evidence):
${push_lines}

Funnel (per-run totals; repeated scans are not unique PRs):
Actionable red/conflicting: $(jq -r '.actionable' <<<"$summary"); selected: $(jq -r '.selected' <<<"$summary")
Repair attempts (dispatch or session reuse): $(jq -r '.attempts' <<<"$summary")
Deferred: busy $(jq -r '.busy_deferred' <<<"$summary"); cooldown $(jq -r '.cooldown_deferred' <<<"$summary")

Exceptions: recovery-blocked $(jq -r '.recovery_blocked' <<<"$summary"); delivery-unconfirmed $(jq -r '.delivery_unconfirmed' <<<"$summary")
Other outcomes: blocked $(jq -r '.blockers' <<<"$summary"), no change $(jq -r '.unchanged' <<<"$summary"), in progress $(jq -r '.in_progress' <<<"$summary"), dispatch failures $(jq -r '.dispatch_failures' <<<"$summary")
Coverage: runs $(jq -r '.runs' <<<"$summary"), discovery observations $(jq -r '.discovered' <<<"$summary"), complete snapshots $(jq -r '.covered_runs' <<<"$summary")/$(jq -r '.runs' <<<"$summary"); native ack observed $(jq -r '.native_ack_observed' <<<"$summary"), missing $(jq -r '.native_ack_missing' <<<"$summary"), untracked/legacy $(jq -r '.native_ack_untracked' <<<"$summary"). Unknown is not zero.

Policy: only explicit red CI or merge conflicts; no merges or force-pushes.
Note: dispatches and session reuse are attempts, never fixes."

should_send() {
  local last
  [[ -f "$STATE_FILE" ]] || return 0
  last="$(jq -r --arg mode "$MODE" '.[$mode].last_sent_at // 0' "$STATE_FILE" 2>/dev/null || printf '0')"
  [[ "$last" =~ ^[0-9]+$ ]] || return 0
  if [[ "$MODE" == "email" ]]; then
    [[ "$last" == 0 || "$(date -d "@$last" +%F)" != "$(date -d "@$NOW" +%F)" ]]
    return
  fi
  (( NOW - last >= WINDOW_SECONDS ))
}

mark_sent() {
  local tmp
  tmp="$(mktemp "$METRICS_DIR/report-state.XXXXXX")"
  if [[ -f "$STATE_FILE" ]]; then
    jq --arg mode "$MODE" --argjson now "$NOW" '.[$mode] = {last_sent_at:$now}' "$STATE_FILE" >"$tmp"
  else
    jq -n --arg mode "$MODE" --argjson now "$NOW" '{($mode): {last_sent_at:$now}}' >"$tmp"
  fi
  mv "$tmp" "$STATE_FILE"
}

curl_config_value() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\n'/\\n}"
  printf '"%s"' "$value"
}

send_smtp_email() (
  local recipient="$1" subject="$2" smtp_user smtp_pass smtp_body
  smtp_user="${PR_GREEN_SMTP_USER:-${EMAIL_USER:-}}"
  smtp_pass="${PR_GREEN_SMTP_PASS:-${EMAIL_PASS:-}}"
  if [[ -z "$smtp_user" || -z "$smtp_pass" ]]; then
    echo "pr-green SMTP fallback unavailable: no SMTP credentials in service environment" >&2
    return 1
  fi
  if [[ "$smtp_user" == *$'\n'* || "$smtp_pass" == *$'\n'* ]]; then
    echo "pr-green SMTP fallback unavailable: SMTP credentials contain a newline" >&2
    return 1
  fi

  # Stream credentials to curl: neither argv nor a temporary file contains them.
  umask 077
  smtp_body="$(mktemp "$METRICS_DIR/smtp-body.XXXXXX")"
  trap 'rm -f "$smtp_body"' EXIT
  printf 'From: %s\r\nTo: %s\r\nSubject: %s\r\n\r\n%s\r\n' \
    "$smtp_user" "$recipient" "$subject" "$body" >"$smtp_body"
  {
    printf 'url = "smtps://smtp.gmail.com:465"\n'
    printf 'user = '; curl_config_value "$smtp_user:$smtp_pass"; printf '\n'
    printf 'mail-from = '; curl_config_value "$smtp_user"; printf '\n'
    printf 'mail-rcpt = '; curl_config_value "$recipient"; printf '\n'
    printf 'upload-file = '; curl_config_value "$smtp_body"; printf '\n'
    printf 'ssl-reqd\nconnect-timeout = 10\nmax-time = 30\nsilent\nshow-error\nfail\n'
  } | curl --config -
)

if [[ "$MODE" == "stdout" ]]; then
  printf '%s\n' "$body"
  exit 0
fi

if ! should_send; then
  echo "pr-green report $MODE suppressed by cadence state" >&2
  exit 0
fi

case "$MODE" in
  slack)
    token="${HERMES_SLACK_BOT_TOKEN:-${SLACK_BOT_TOKEN:-}}"
    if [[ -z "$token" ]]; then
      echo "pr-green Slack report not sent: no Slack bot token in service environment" >&2
      exit 1
    fi
    channel="${PR_GREEN_SLACK_CHANNEL_ID:-C0AJQ5M0A0Y}"
    if { printf 'header = '; curl_config_value "Authorization: Bearer $token"; printf '\n'; } \
      | curl --config - --fail --silent --show-error --max-time 20 -X POST https://slack.com/api/chat.postMessage \
      -H 'Content-Type: application/json; charset=utf-8' \
      --data "$(jq -n --arg channel "$channel" --arg text "$body" '{channel:$channel,text:$text}')" \
      | jq -e '.ok == true' >/dev/null; then
      mark_sent
    else
      echo "pr-green Slack report delivery failed; cadence state was not advanced" >&2
      exit 1
    fi
    ;;
  email)
    account="${PR_GREEN_GMAIL_ACCOUNT:-jleechan@gmail.com}"
    recipient="${PR_GREEN_EMAIL_TO:-jleechan@gmail.com}"
    subject="PR green repair report — $(date -d "@$NOW" '+%Y-%m-%d')"
    if command -v gog >/dev/null && gog gmail send -a "$account" --to "$recipient" \
      --subject "$subject" --body "$body" --no-input; then
      mark_sent
    elif send_smtp_email "$recipient" "$subject"; then
      mark_sent
    else
      echo "pr-green email report delivery failed for $account; cadence state was not advanced" >&2
      exit 1
    fi
    ;;
esac

printf '%s\n' "$body"
