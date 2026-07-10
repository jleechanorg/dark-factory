#!/usr/bin/env bash
# libnotify-slack.sh — minimal Slack-poster for the dark-factory daemon.
#
# Sourced by factory-af-tick.sh + factory-ao-remediate.sh when SLACK
# notifications are configured. Designed to fail soft: when
# HERMES_SLACK_BOT_TOKEN or FACTORY_SLACK_CHANNEL_ID is unset, every call
# is a no-op so the daemon keeps working in environments without Slack.
#
# Functions:
#   slack_capable           echo "1" if both env vars are present, else "0".
#   slack_post <text>       POST a top-level message to FACTORY_SLACK_CHANNEL_ID.
#                           Falls back to thread_ts=$FACTORY_SLACK_THREAD_TS if
#                           set (so tick output stays in a single topic).
#
# Env:
#   HERMES_SLACK_BOT_TOKEN  xoxb-... token (also falls back to SLACK_BOT_TOKEN).
#   FACTORY_SLACK_CHANNEL_ID  C0... channel (e.g. #factory).
#   FACTORY_SLACK_THREAD_TS  optional — reply in this thread instead of top-level.
#   FACTORY_SLACK_ASYNC=1   post in background (default 1; safe for tick loop).
#   FACTORY_SLACK_TIMEOUT   curl timeout seconds (default 5).
#
# Pattern source: ~/bin/monitor-agent.sh line ~2519 (chat.postMessage curl).
set -u

slack_token() {
    printf '%s' "${HERMES_SLACK_BOT_TOKEN:-${SLACK_BOT_TOKEN:-}}"
}

slack_channel() {
    printf '%s' "${FACTORY_SLACK_CHANNEL_ID:-}"
}

slack_capable() {
    if [ -n "$(slack_token)" ] && [ -n "$(slack_channel)" ]; then
        echo 1
    else
        echo 0
    fi
}

# POST a single message. Uses jq-free shell JSON escaping via python3
# (python3 is already required by every daemon script). Fail soft.
slack_post() {
    local text="${1:-}"
    [ -n "$text" ] || return 0
    if [ "$(slack_capable)" != "1" ]; then
        return 0
    fi
    local token channel payload thread_ts
    token="$(slack_token)"
    channel="$(slack_channel)"
    thread_ts="${FACTORY_SLACK_THREAD_TS:-}"
    payload="$(python3 -c '
import json, sys
d = {"channel": sys.argv[1], "text": sys.argv[2]}
if sys.argv[3]:
    d["thread_ts"] = sys.argv[3]
print(json.dumps(d))' "$channel" "$text" "$thread_ts")"
    if [ "${FACTORY_SLACK_ASYNC:-1}" = "1" ]; then
        (
            curl -sS --max-time "${FACTORY_SLACK_TIMEOUT:-5}" \
                 -X POST "https://slack.com/api/chat.postMessage" \
                 -H "Authorization: Bearer ${token}" \
                 -H "Content-Type: application/json; charset=utf-8" \
                 --data "$payload" >/dev/null 2>&1 \
              || echo "[libnotify-slack] slack_post failed (channel=${channel})" >&2
        ) &
        return 0
    fi
    curl -sS --max-time "${FACTORY_SLACK_TIMEOUT:-5}" \
         -X POST "https://slack.com/api/chat.postMessage" \
         -H "Authorization: Bearer ${token}" \
         -H "Content-Type: application/json; charset=utf-8" \
         --data "$payload" >/dev/null 2>&1 \
      || echo "[libnotify-slack] slack_post failed (channel=${channel})" >&2
    return 0
}

# PUBLIC: helper for short announcement strings. Falls back to a noop when
# slack is not configured so callers don't have to gate on slack_capable.
slack_announce() {
    local prefix="${1:-}"; shift || true
    if [ "$(slack_capable)" != "1" ]; then
        return 0
    fi
    local body="${prefix}"
    if [ "$#" -gt 0 ]; then
        body="${prefix} — $*"
    fi
    slack_post "$body"
}
