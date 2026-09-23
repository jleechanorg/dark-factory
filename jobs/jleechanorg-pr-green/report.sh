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
[[ -f "$RUNS_FILE" ]] || : >"$RUNS_FILE"
[[ -f "$OUTCOMES_FILE" ]] || : >"$OUTCOMES_FILE"

summary="$(jq -s --slurpfile outcomes "$OUTCOMES_FILE" --argjson since "$SINCE" '
  def recent: map(select((.ts // 0) >= $since));
  (recent) as $runs |
  ($outcomes | recent) as $outcomes |
  {
    runs: ($runs | length),
    discovered: ($runs | map(.discovered // 0) | add // 0),
    analyzed: ($runs | map(.analyzed // 0) | add // 0),
    actionable: ($runs | map(.actionable // 0) | add // 0),
    selected: ($runs | map(.selected // 0) | add // 0),
    attempts: ($runs | map(.attempted // 0) | add // 0),
    outcome_records: ($outcomes | length),
    confirmed_fixes: ($outcomes | map(select(.result == "fixed" and (.verified == true))) | length),
    blockers: ($outcomes | map(select(.result == "blocked")) | length),
    unchanged: ($outcomes | map(select(.result == "no_change")) | length),
    in_progress: ($outcomes | map(select(.result == "in_progress")) | length),
    dispatch_failures: ($outcomes | map(select(.result == "dispatch_failed")) | length)
  }
' "$RUNS_FILE")"

body="PR green repair report (last ${WINDOW_HOURS}h)

Runs: $(jq -r '.runs' <<<"$summary")
Eligible recently updated PRs discovered: $(jq -r '.discovered' <<<"$summary")
PRs analyzed for red CI/conflict: $(jq -r '.analyzed' <<<"$summary")
Actionable red/conflicting: $(jq -r '.actionable' <<<"$summary")
Selected for repair: $(jq -r '.selected' <<<"$summary")
Repair attempts (dispatch or session reuse): $(jq -r '.attempts' <<<"$summary")

Durable PR outcomes: $(jq -r '.outcome_records' <<<"$summary")
Confirmed fixes (verified): $(jq -r '.confirmed_fixes' <<<"$summary")
Blocked: $(jq -r '.blockers' <<<"$summary")
No change: $(jq -r '.unchanged' <<<"$summary")
In progress: $(jq -r '.in_progress' <<<"$summary")
Dispatch failures: $(jq -r '.dispatch_failures' <<<"$summary")

Policy: only explicit red CI or merge conflicts; no merges or force-pushes.
Note: dispatches and session reuse are attempts, never fixes."

should_send() {
  local last
  [[ -f "$STATE_FILE" ]] || return 0
  last="$(jq -r --arg mode "$MODE" '.[$mode].last_sent_at // 0' "$STATE_FILE" 2>/dev/null || printf '0')"
  [[ "$last" =~ ^[0-9]+$ ]] || return 0
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

send_smtp_email() {
  local recipient="$1" subject="$2" smtp_user smtp_pass smtp_config smtp_body
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

  # Keep the app password out of argv and remove the 0600 transport files on
  # every return path. curl's config is used only because it keeps credentials
  # out of process listings; it is not persisted with the cadence state.
  umask 077
  smtp_config="$(mktemp "$METRICS_DIR/smtp-config.XXXXXX")"
  smtp_body="$(mktemp "$METRICS_DIR/smtp-body.XXXXXX")"
  trap 'rm -f "$smtp_config" "$smtp_body"' RETURN
  printf 'From: %s\r\nTo: %s\r\nSubject: %s\r\n\r\n%s\r\n' \
    "$smtp_user" "$recipient" "$subject" "$body" >"$smtp_body"
  {
    printf 'url = "smtps://smtp.gmail.com:465"\n'
    printf 'user = "%s:%s"\n' "$smtp_user" "$smtp_pass"
    printf 'mail-from = "%s"\n' "$smtp_user"
    printf 'mail-rcpt = "%s"\n' "$recipient"
    printf 'upload-file = "%s"\n' "$smtp_body"
    printf 'ssl-reqd\nconnect-timeout = 10\nmax-time = 30\nsilent\nshow-error\nfail\n'
  } >"$smtp_config"
  curl --config "$smtp_config"
}

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
      exit 0
    fi
    channel="${PR_GREEN_SLACK_CHANNEL_ID:-C0AJQ5M0A0Y}"
    if curl --fail --silent --show-error --max-time 20 -X POST https://slack.com/api/chat.postMessage \
      -H "Authorization: Bearer $token" -H 'Content-Type: application/json; charset=utf-8' \
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
