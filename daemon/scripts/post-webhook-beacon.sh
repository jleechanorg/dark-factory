#!/usr/bin/env bash
# post-webhook-beacon.sh — emit a 1-line /af milestone beacon to #factory.
#
# Called by github-webhook-listener.sh when a push event hits a whitelisted
# GH_REPOS entry. Builds a short summary line including the actor, branch,
# commit (short SHA), and compares URL, then posts via libnotify-slack.sh.
#
# Env (injected by the caller — github-webhook-listener.sh):
#   HERMES_SLACK_BOT_TOKEN     required (else post is a no-op)
#   FACTORY_SLACK_CHANNEL_ID   C0... channel (defaults to C0BGEC77EP4)
#   WEBHOOK_REPO               full_name (e.g. jleechanorg/worldarchitect.ai)
#   WEBHOOK_REF                ref (e.g. refs/heads/main)
#   WEBHOOK_BRANCH             short branch (e.g. main) — derived
#   WEBHOOK_PUSHER             pusher.name
#   WEBHOOK_HEAD_SHA           head_commit.id
#   WEBHOOK_HEAD_MESSAGE       head_commit.message (first line, truncated)
#   WEBHOOK_COMPARE            compare URL (e.g. https://github.com/.../compare/old...new)
#
# Exit codes:
#   0  always (fail-soft — the listener should not crash on bad payloads)
set -u

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="${WEBHOOK_REPO:-unknown/repo}"
BRANCH="${WEBHOOK_BRANCH:-${WEBHOOK_REF:-unknown}}"
PUSHER="${WEBHOOK_PUSHER:-unknown}"
SHA="${WEBHOOK_HEAD_SHA:-0000000}"
MSG="${WEBHOOK_HEAD_MESSAGE:-}"
COMPARE="${WEBHOOK_COMPARE:-}"

# Truncate the commit message to keep beacons short (Slack mobile preview).
case "$MSG" in
    *"
"*)
        first_line="${MSG%%$'\n'*}"
        ;;
    *)
        first_line="$MSG"
        ;;
esac
if [ "${#first_line}" -gt 80 ]; then
    first_line="${first_line:0:77}..."
fi

# Build the beacon text.
if [ -n "$COMPARE" ]; then
    compare_note=" <${COMPARE}>"
else
    compare_note=""
fi

if [ -n "$first_line" ]; then
    beacon=":factory: /af milestone — \`${REPO}\` @ \`${BRANCH}\` ${SHA:0:8} by ${PUSHER} — ${first_line}${compare_note}"
else
    beacon=":factory: /af milestone — \`${REPO}\` @ \`${BRANCH}\` ${SHA:0:8} by ${PUSHER}${compare_note}"
fi

# Source libnotify-slack.sh (fail-soft).
if [ -r "$SCRIPT_DIR/libnotify-slack.sh" ]; then
    # shellcheck disable=SC1091
    . "$SCRIPT_DIR/libnotify-slack.sh"
else
    echo "[post-webhook-beacon] libnotify-slack.sh not found at $SCRIPT_DIR/libnotify-slack.sh" >&2
    exit 0
fi

# Always log locally.
echo "[post-webhook-beacon] $(date -u +%Y-%m-%dT%H:%M:%SZ) repo=$REPO branch=$BRANCH sha=$SHA pusher=$PUSHER"

# Post (default async). slack_post is fail-soft; exit 0 always.
slack_post "$beacon" || true
exit 0