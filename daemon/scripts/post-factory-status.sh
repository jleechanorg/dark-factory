#!/usr/bin/env bash
# post-factory-status.sh — every-5-min /af status beacon to #factory.
#
# The factory-af-tick.sh tick already posts per-tick beacons when dispatch
# activity happens, but the user explicitly wants a heartbeat regardless of
# whether anything was dispatched. This script posts a 4-line status snapshot
# (dispatched / beads-active / ao-sessions / uptime) to #factory via the
# existing libnotify-slack.sh helper.
#
# Scheduling: ai.dark-factory.status-cron.plist.template fires this script
# every 300s (5 min) via launchd StartInterval. launchd-wrapper.sh sources the
# user's interactive login env (PATH, br, gh, sqlite3) before exec so that
# the bead-CLI calls in this script actually resolve — the same PATH problem
# documented in bead jleechan-v2wv applies here.
#
# Usage:
#   post-factory-status.sh            # POST to #factory (no-op if Slack unset)
#   post-factory-status.sh --dry-run  # print intended message, no Slack call
#
# Env (with defaults):
#   FACTORY_DARK_FACTORY_HOME   repo root (default $HOME/projects/dark-factory)
#   AFD_DB                      CXDB sqlite (default ~/.dark-factory/daemon-cxdb.sqlite)
#   STATUS_BEACON_ASYNC=1       post in background (default 1; safe for cron)
set -euo pipefail

REPO_ROOT="${FACTORY_DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
DB="${AFD_DB:-$HOME/.dark-factory/daemon-cxdb.sqlite}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

DRY_RUN=0
case "${1:-}" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help)
        sed -n '2,20p' "$0"
        exit 0
        ;;
esac

# Source libnotify-slack.sh (fail-soft: no-op when env unset).
if [ -r "$SCRIPT_DIR/libnotify-slack.sh" ]; then
    # shellcheck disable=SC1091
    . "$SCRIPT_DIR/libnotify-slack.sh"
else
    echo "[post-factory-status] libnotify-slack.sh not found at $SCRIPT_DIR/libnotify-slack.sh" >&2
    exit 0
fi

# --- gather metrics --------------------------------------------------------

# 1. Last-tick dispatched count from CXDB.
dispatched=0
last_dispatch_age="n/a"
if [ -r "$DB" ] && command -v sqlite3 >/dev/null 2>&1; then
    dispatched="$(sqlite3 "$DB" "SELECT COUNT(*) FROM task_dispatched WHERE ts >= strftime('%s','now') - 300;" 2>/dev/null || echo 0)"
    last_dispatch_age="$(sqlite3 "$DB" "SELECT COALESCE((strftime('%s','now') - MAX(ts)), 'n/a') FROM task_dispatched;" 2>/dev/null || echo 'n/a')"
fi

# 2. Open beads (status != closed) count.
beads_active=0
if [ -x "${BR_BIN:-$HOME/.cargo/bin/br}" ] || command -v br >/dev/null 2>&1; then
    br_bin="${BR_BIN:-$(command -v br || true)}"
    if [ -n "$br_bin" ] && [ -d "$REPO_ROOT/.beads" ]; then
        # br list --status open --json is the canonical source-of-truth.
        # Fall back to "0" if br errors (e.g. db locked) so the beacon still posts.
        beads_active="$(BR_DB="$REPO_ROOT/.beads/beads.db" "$br_bin" list --status open --json 2>/dev/null \
            | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d) if isinstance(d,list) else 0)' 2>/dev/null \
            || echo 0)"
    fi
fi

# 3. Active AO sessions — placeholder for AO CLI integration.
#    Until AO exposes a stable 'list --status active' query we report 'unknown'
#    rather than fabricating a number; this preserves the 'do not lie' invariant.
ao_sessions="unknown"

# 4. Uptime of the af-tick launchd agent (label ai.dark-factory.af-tick).
#    Falls back to 'not-loaded' if launchctl can't find it. We get the PID
#    from launchctl then read /proc-style elapsed time via `ps -o etime=`.
af_uptime="not-loaded"
if command -v launchctl >/dev/null 2>&1; then
    af_pid="$(launchctl print "gui/$UID/ai.dark-factory.af-tick" 2>/dev/null \
        | awk -F'= ' '/^[[:space:]]*pid = /{gsub(/[[:space:]]/,"",$2); print $2; exit}')"
    if [ -n "$af_pid" ] && [ "$af_pid" != "0" ] && command -v ps >/dev/null 2>&1; then
        # ps -o etime= prints e.g. "12-03:45:21" (days-hh:mm:ss) or "03:45:21".
        af_uptime="$(ps -o etime= -p "$af_pid" 2>/dev/null | tr -d ' ' || true)"
        [ -z "$af_uptime" ] && af_uptime="not-loaded"
    fi
fi

# 5. Timestamp + git HEAD of the dark-factory repo (operator visibility).
stamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
head_sha="$(git -C "$REPO_ROOT" rev-parse --short=8 HEAD 2>/dev/null || echo unknown)"

message=$(printf ':factory: /af status beacon — %s
- dispatched (5m): %s   last: %s
- beads open:      %s
- ao sessions:     %s
- af-tick uptime:  %s
- head:            %s' \
    "$stamp" "$dispatched" "$last_dispatch_age" "$beads_active" "$ao_sessions" "$af_uptime" "$head_sha")

if [ "$DRY_RUN" -eq 1 ]; then
    echo "----- DRY RUN: intended Slack message -----"
    echo "$message"
    echo "----- end -----"
    exit 0
fi

# Post (fail-soft: slack_post returns 0 even when Slack is not configured).
if [ "${STATUS_BEACON_ASYNC:-1}" = "1" ]; then
    (slack_post "$message" &) >/dev/null 2>&1 || true
else
    slack_post "$message" || true
fi

# Always log locally so operators can grep the cron even if Slack is down.
echo "[post-factory-status] $stamp head=$head_sha dispatched=$dispatched beads=$beads_active ao=$ao_sessions uptime=$af_uptime"