#!/usr/bin/env bash
# Idempotent installer for dark-factory launchd agents.
#
# Discovers every *.plist.template under daemon/launchd/, substitutes
# @HOME@, @TICK_INTERVAL@, @STATUS_INTERVAL@, @FACTORY_SLACK_CHANNEL_ID@,
# @HERMES_SLACK_BOT_TOKEN@, @GITHUB_WEBHOOK_SECRET@, and @GH_REPOS@
# (the latter for the GitHub webhook plist), copies the result to
# $HOME/Library/LaunchAgents/<basename>.plist with mode 0644, and
# bootstrap/bootout handles the launchd lifecycle.
#
# Usage:
#   ./install-launchagents.sh [--dry-run] [--uninstall] [--label <name>]
#
# Flags:
#   --dry-run          echo actions without executing
#   --uninstall        bootout and remove installed plists
#   --label <name>     limit install/uninstall to a single label
#                      (e.g. ai.dark-factory.af-tick); repeatable
#
# Environment:
#   AFD_TICK_INTERVAL_SEC     optional override for the af-tick StartInterval
#                             (default 240). Read by ai.dark-factory.af-tick.plist.template.
#   AFD_STATUS_INTERVAL_SEC   optional override for the status-cron StartInterval
#                             (default 300). Read by ai.dark-factory.status-cron.plist.template.
#   HERMES_SLACK_BOT_TOKEN    optional Slack bot token (xoxb-...). Read from the
#                             operator's environment. If unset, falls back to
#                             /usr/local/etc/hermes-token (mode 0600, $USER-readable).
#                             If still empty, the script REFUSES to write a plist
#                             with an empty token — it skips Slack-using templates
#                             with a clear error and continues with non-Slack ones.
#                             NEVER commit a real token; this script never reads
#                             tokens from disk outside the operator-owned fallback.
#   FACTORY_SLACK_CHANNEL_ID  optional override for the #factory channel id
#                             (default C0BGEC77EP4 — the channel created on
#                             2026-07-09 in team T09FXQ4LCQP).
#   GITHUB_WEBHOOK_SECRET     required by ai.dark-factory.github-webhook.plist
#                             (HMAC-SHA256 secret shared with GitHub). Same
#                             resolution rules as HERMES_SLACK_BOT_TOKEN.
#                             Empty -> the webhook plist install fails with exit 5.
#   GH_REPOS                  comma-separated list of full_name repos accepted
#                             by the webhook listener (default
#                             'jleechanorg/worldarchitect.ai,jleechanorg/dark-factory').
#                             Empty string means 'accept all repos' (operator opt-in).
#
# Exit codes:
#   0  success
#   2  invalid argument
#   3  template contains unsupported placeholders
#   4  bootstrap failed (only when not --dry-run)
#   5  required secret/token missing (Slack token OR GitHub webhook secret)

set -euo pipefail

# Resolve repo root from this script's location (../ from daemon/launchd OR
# directly when this script lives at the repo root). Both layouts work.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if [ -d "$SCRIPT_DIR/daemon/launchd" ]; then
    REPO_ROOT="$SCRIPT_DIR"
    PLIST_DIR="$SCRIPT_DIR/daemon/launchd"
else
    # Fallback: installer was placed under daemon/launchd/ (legacy layout).
    REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
    PLIST_DIR="$SCRIPT_DIR"
fi

LAUNCH_AGENTS_DIR="$HOME/Library/LaunchAgents"
LOG_DIR="$HOME/Library/Logs/dark-factory"
LAUNCHCTL_DOMAIN="gui/$UID"

DRY_RUN=0
UNINSTALL=0
LABELS=()
i=1
while [ "$i" -le "$#" ]; do
    arg="${@:$i:1}"
    case "$arg" in
        --dry-run) DRY_RUN=1; i=$((i + 1)) ;;
        --uninstall) UNINSTALL=1; i=$((i + 1)) ;;
        --label)
            i=$((i + 1))
            if [ "$i" -gt "$#" ]; then
                echo "[error] --label requires a value" >&2
                exit 2
            fi
            LABELS+=("${@:$i:1}")
            i=$((i + 1))
            ;;
        -h|--help)
            cat <<EOF
Usage: $0 [--dry-run] [--uninstall] [--label <name>]

Installs every *.plist.template under $PLIST_DIR to
$LAUNCH_AGENTS_DIR/<label>.plist, then bootstraps it via launchctl.

Defaults to installing every template. Pass --label to limit to a
single label (repeatable). Pass --uninstall to remove them instead.
EOF
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

# (legacy two-pass parser removed; single-pass loop above handles all flags.)

run() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] %s\n' "$*"
    else
        "$@"
    fi
}

ensure_log_dir() {
    if [ ! -d "$LOG_DIR" ]; then
        run mkdir -p "$LOG_DIR"
    fi
}

# Refuse to render any template that contains placeholders other than the
# installer-managed set. Catches the qw5-pilot-dispatch.plist.template case
# where @YEAR@/@MONTH@/etc are meant to be filled by an operator, not the
# installer. Operators of those one-shot timers must render them by hand
# and copy to ~/Library/LaunchAgents.
#
# Installer-managed placeholders:
#   @HOME@                       always substituted from $HOME
#   @TICK_INTERVAL@               substituted from AFD_TICK_INTERVAL_SEC
#   @STATUS_INTERVAL@             substituted from AFD_STATUS_INTERVAL_SEC
#   @FACTORY_SLACK_CHANNEL_ID@    substituted from $FACTORY_SLACK_CHANNEL_ID
#                                 (default C0BGEC77EP4 — public channel id)
#   @HERMES_SLACK_BOT_TOKEN@      substituted from $HERMES_SLACK_BOT_TOKEN
#                                 or /usr/local/etc/hermes-token (operator-only);
#                                 if both are empty, templates that require it
#                                 are skipped with a clear error.
#   @GITHUB_WEBHOOK_SECRET@       substituted from $GITHUB_WEBHOOK_SECRET
#                                 or /usr/local/etc/hermes-token (same fallback
#                                 chain as the Slack token); required by the
#                                 github-webhook plist (HMAC validation will
#                                 reject every payload if it's empty).
#   @GH_REPOS@                    substituted from $GH_REPOS (default
#                                 'jleechanorg/worldarchitect.ai,jleechanorg/dark-factory');
#                                 empty string = 'accept all'.
ALLOWED_PLACEHOLDERS='@HOME@|@TICK_INTERVAL@|@STATUS_INTERVAL@|@FACTORY_SLACK_CHANNEL_ID@|@HERMES_SLACK_BOT_TOKEN@|@GITHUB_WEBHOOK_SECRET@|@GH_REPOS@'
validate_template_placeholders() {
    local tpl="$1"
    local bad
    bad="$(grep -oE '@[A-Z][A-Z0-9_]+@' "$tpl" | sort -u | grep -vE "^($ALLOWED_PLACEHOLDERS)$" || true)"
    if [ -n "$bad" ]; then
        echo "[skip] $tpl contains non-installer placeholders ($bad); render manually" >&2
        return 1
    fi
    return 0
}

# Resolve the Slack token. Returns 0 with token on stdout if found, 1 otherwise.
# Sources (first hit wins): $HERMES_SLACK_BOT_TOKEN, /usr/local/etc/hermes-token.
resolve_slack_token() {
    if [ -n "${HERMES_SLACK_BOT_TOKEN:-}" ]; then
        printf '%s' "$HERMES_SLACK_BOT_TOKEN"
        return 0
    fi
    local fallback="/usr/local/etc/hermes-token"
    if [ -r "$fallback" ]; then
        # Trim trailing whitespace/newlines; never echo the token to logs.
        local tok
        tok="$(head -n 1 "$fallback" 2>/dev/null | tr -d '\r\n' || true)"
        if [ -n "$tok" ]; then
            printf '%s' "$tok"
            return 0
        fi
    fi
    return 1
}

# Resolve the GitHub webhook secret. Same fallback chain as resolve_slack_token.
# Operators can set GITHUB_WEBHOOK_SECRET directly, or share /usr/local/etc/hermes-token.
resolve_github_webhook_secret() {
    if [ -n "${GITHUB_WEBHOOK_SECRET:-}" ]; then
        printf '%s' "$GITHUB_WEBHOOK_SECRET"
        return 0
    fi
    local fallback="/usr/local/etc/hermes-token"
    if [ -r "$fallback" ]; then
        local tok
        tok="$(head -n 1 "$fallback" 2>/dev/null | tr -d '\r\n' || true)"
        if [ -n "$tok" ]; then
            printf '%s' "$tok"
            return 0
        fi
    fi
    return 1
}

# Default #factory channel id. Channel ids are not secrets (they appear in
# Slack URLs), so it's safe to commit. Override via $FACTORY_SLACK_CHANNEL_ID.
DEFAULT_FACTORY_SLACK_CHANNEL_ID="C0BGEC77EP4"

# Default GH_REPOS allow-list. Two repos the operator actively drives via /af.
# Operators can override via $GH_REPOS; empty string means 'accept all'.
DEFAULT_GH_REPOS="jleechanorg/worldarchitect.ai,jleechanorg/dark-factory"

# Resolve the tick interval from env, defaulting to 240s. Validate as positive int.
TICK_INTERVAL="${AFD_TICK_INTERVAL_SEC:-240}"
case "$TICK_INTERVAL" in
    ''|*[!0-9]*)
        echo "[error] AFD_TICK_INTERVAL_SEC must be a positive integer (got: $TICK_INTERVAL)" >&2
        exit 2
        ;;
esac
if [ "$TICK_INTERVAL" -lt 10 ]; then
    echo "[error] AFD_TICK_INTERVAL_SEC=$TICK_INTERVAL is too low (minimum 10s)" >&2
    exit 2
fi

# Resolve the status-cron interval from env, defaulting to 300s. Validate as positive int.
STATUS_INTERVAL="${AFD_STATUS_INTERVAL_SEC:-300}"
case "$STATUS_INTERVAL" in
    ''|*[!0-9]*)
        echo "[error] AFD_STATUS_INTERVAL_SEC must be a positive integer (got: $STATUS_INTERVAL)" >&2
        exit 2
        ;;
esac
if [ "$STATUS_INTERVAL" -lt 30 ]; then
    echo "[error] AFD_STATUS_INTERVAL_SEC=$STATUS_INTERVAL is too low (minimum 30s)" >&2
    exit 2
fi

discover_plists() {
    find "$PLIST_DIR" -maxdepth 1 -type f -name '*.plist.template' \
        | while read -r tpl; do
            base="$(basename "$tpl" .plist.template)"
            echo "${base}|${tpl}"
        done
}

# Print "label" lines from LABELS array, one per line. Empty = no filter.
label_filter_active() {
    [ "${#LABELS[@]}" -gt 0 ]
}

label_allowed() {
    local label="$1"
    if ! label_filter_active; then
        return 0
    fi
    for l in "${LABELS[@]}"; do
        [ "$l" = "$label" ] && return 0
    done
    return 1
}

install_one() {
    local label="$1"
    local tpl="$2"
    local target="$LAUNCH_AGENTS_DIR/${label}.plist"

    if ! validate_template_placeholders "$tpl"; then
        return 0  # Skip, not error; already logged.
    fi

    # Refuse to render a template that requires a Slack token if none is
    # available. Operators can either set $HERMES_SLACK_BOT_TOKEN or place
    # it in /usr/local/etc/hermes-token.
    local needs_token=0
    if grep -q '@HERMES_SLACK_BOT_TOKEN@' "$tpl"; then
        needs_token=1
    fi
    local needs_webhook_secret=0
    if grep -q '@GITHUB_WEBHOOK_SECRET@' "$tpl"; then
        needs_webhook_secret=1
    fi
    local slack_token=""
    local webhook_secret=""
    local channel_id="${FACTORY_SLACK_CHANNEL_ID:-$DEFAULT_FACTORY_SLACK_CHANNEL_ID}"
    # GH_REPOS may be intentionally empty (= 'accept all'); use ${var-} so
    # an unset env var defaults to the constant rather than to "".
    local gh_repos="${GH_REPOS-$DEFAULT_GH_REPOS}"

    if [ "$needs_token" -eq 1 ]; then
        if ! slack_token="$(resolve_slack_token)"; then
            if [ "$DRY_RUN" -eq 1 ]; then
                printf '[dry-run] [skip] %s requires HERMES_SLACK_BOT_TOKEN; would fail in real install\n' "$label" >&2
                return 0
            fi
            echo "[error] $label requires HERMES_SLACK_BOT_TOKEN (set env or place in /usr/local/etc/hermes-token mode 0600)" >&2
            exit 5
        fi
        # Never log the token value itself.
        if [ "$DRY_RUN" -eq 1 ]; then
            printf '[dry-run] resolved HERMES_SLACK_BOT_TOKEN (length=%d) for %s\n' "${#slack_token}" "$label"
        fi
    fi

    if [ "$needs_webhook_secret" -eq 1 ]; then
        if ! webhook_secret="$(resolve_github_webhook_secret)"; then
            if [ "$DRY_RUN" -eq 1 ]; then
                printf '[dry-run] [skip] %s requires GITHUB_WEBHOOK_SECRET; would fail in real install\n' "$label" >&2
                return 0
            fi
            echo "[error] $label requires GITHUB_WEBHOOK_SECRET (set env or place in /usr/local/etc/hermes-token mode 0600)" >&2
            exit 5
        fi
        if [ "$DRY_RUN" -eq 1 ]; then
            printf '[dry-run] resolved GITHUB_WEBHOOK_SECRET (length=%d) for %s\n' "${#webhook_secret}" "$label"
        fi
    fi

    ensure_log_dir

    # Render template -> target via sed. Use % as delimiter so $HOME |'s survive.
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] sed -e s%%@HOME@%%%s%%g -e s%%@TICK_INTERVAL@%%%s%%g -e s%%@STATUS_INTERVAL@%%%s%%g -e s%%@FACTORY_SLACK_CHANNEL_ID@%%%s%%g -e s%%@HERMES_SLACK_BOT_TOKEN@%%<slack_token len=%d>%%g -e s%%@GITHUB_WEBHOOK_SECRET@%%<webhook_secret len=%d>%%g -e s%%@GH_REPOS@%%%s%%g %s > %s\n' \
            "$HOME" "$TICK_INTERVAL" "$STATUS_INTERVAL" "$channel_id" "${#slack_token}" "${#webhook_secret}" "$gh_repos" "$tpl" "$target"
    else
        sed -e "s%@HOME@%${HOME}%g" \
            -e "s%@TICK_INTERVAL@%${TICK_INTERVAL}%g" \
            -e "s%@STATUS_INTERVAL@%${STATUS_INTERVAL}%g" \
            -e "s%@FACTORY_SLACK_CHANNEL_ID@%${channel_id}%g" \
            -e "s%@HERMES_SLACK_BOT_TOKEN@%${slack_token}%g" \
            -e "s%@GITHUB_WEBHOOK_SECRET@%${webhook_secret}%g" \
            -e "s%@GH_REPOS@%${gh_repos}%g" "$tpl" > "$target"
        # launchctl rejects group/other-writable plists; force 0644 regardless of umask.
        chmod 0644 "$target"
    fi

    # Best-effort bootout (ignore if not loaded).
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] launchctl bootout %s %s 2>/dev/null || true\n' "$LAUNCHCTL_DOMAIN" "$target"
    else
        launchctl bootout "$LAUNCHCTL_DOMAIN" "$target" 2>/dev/null || true
    fi

    # Bootstrap.
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] launchctl bootstrap %s %s\n' "$LAUNCHCTL_DOMAIN" "$target"
    else
        if ! launchctl bootstrap "$LAUNCHCTL_DOMAIN" "$target"; then
            echo "[error] bootstrap failed for $target" >&2
            return 4
        fi
    fi
}

uninstall_one() {
    local label="$1"
    local target="$LAUNCH_AGENTS_DIR/${label}.plist"

    if [ -f "$target" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
            printf '[dry-run] launchctl bootout %s %s\n' "$LAUNCHCTL_DOMAIN" "$target"
            printf '[dry-run] rm %s\n' "$target"
        else
            launchctl bootout "$LAUNCHCTL_DOMAIN" "$target" 2>/dev/null || true
            rm "$target"
        fi
    else
        echo "[skip] $target not present"
    fi
}

# Always create the LaunchAgents dir (idempotent).
run mkdir -p "$LAUNCH_AGENTS_DIR"

while IFS='|' read -r label tpl; do
    [ -n "$label" ] || continue
    label_allowed "$label" || continue
    if [ "$UNINSTALL" -eq 1 ]; then
        uninstall_one "$label"
    else
        install_one "$label" "$tpl"
    fi
done < <(discover_plists)

if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] complete"
elif [ "$UNINSTALL" -eq 1 ]; then
    echo "Uninstalled dark-factory launchd agents"
else
    echo "Installed dark-factory launchd agents:"
    launchctl list | grep -i dark-factory || echo "(none currently loaded)"
fi