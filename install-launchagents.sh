#!/usr/bin/env bash
# Idempotent installer for dark-factory launchd agents.
#
# Discovers every *.plist.template under daemon/launchd/, substitutes
# only @HOME@ (any other placeholder aborts the install of that template
# with a clear error rather than rendering a broken plist), copies the
# result to $HOME/Library/LaunchAgents/<basename>.plist with mode 0644,
# and bootstrap/bootout handles the launchd lifecycle.
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
#   AFD_TICK_INTERVAL_SEC   optional override for the af-tick StartInterval
#                           (default 240). Read by ai.dark-factory.af-tick.plist.template.
#
# Exit codes:
#   0  success
#   2  invalid argument
#   3  template contains unsupported placeholders
#   4  bootstrap failed (only when not --dry-run)

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
# installer-managed set (currently @HOME@ and @TICK_INTERVAL@). Catches the
# qw5-pilot-dispatch.plist.template case where @YEAR@/@MONTH@/etc are meant
# to be filled by an operator, not the installer. Operators of those one-shot
# timers must render them by hand and copy to ~/Library/LaunchAgents.
ALLOWED_PLACEHOLDERS='@HOME@|@TICK_INTERVAL@'
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

    ensure_log_dir

    # Render template -> target via sed. Use % as delimiter so $HOME |'s survive.
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] sed -e s%%@HOME@%%%s%%g -e s%%@TICK_INTERVAL@%%%s%%g %s > %s\n' \
            "$HOME" "$TICK_INTERVAL" "$tpl" "$target"
    else
        sed -e "s%@HOME@%${HOME}%g" \
            -e "s%@TICK_INTERVAL@%${TICK_INTERVAL}%g" "$tpl" > "$target"
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