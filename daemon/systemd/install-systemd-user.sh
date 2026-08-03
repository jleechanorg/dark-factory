#!/usr/bin/env bash
# Idempotent systemd --user installer for the dark-factory user units.
#
# Discovers every `*.service.template` in this directory and installs the
# matching `*.service` (plus the sibling `*.timer` if a corresponding
# `*.timer.template` exists). The Rust auto-factory daemon unit
# (ai.dark-factory.daemon.service) is built before install unless
# --skip-build is passed.
#
# Usage:
#   daemon/systemd/install-systemd-user.sh [--dry-run] [--uninstall] [--skip-build] [--render-only] [--repo PATH] [--label LABEL]
#
# --label is repeatable; templates whose `<basename>` is not in the list
# are skipped (matches the launchd installer convention).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DEFAULT_REPO="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO="${DARK_FACTORY_HOME:-$DEFAULT_REPO}"
UNIT_DIR="${SYSTEMD_USER_DIR:-$HOME/.config/systemd/user}"
LOG_DIR="$HOME/Library/Logs/dark-factory"
INSTALL_USER="${USER:-$(id -un)}"
DRY_RUN=0
UNINSTALL=0
SKIP_BUILD=0
RENDER_ONLY=0
LABELS=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        --render-only) RENDER_ONLY=1; shift ;;
        --repo)
            [ "${2:-}" ] || { echo "--repo requires a path" >&2; exit 2; }
            REPO="$2"
            shift 2
            ;;
        --label)
            [ "${2:-}" ] || { echo "--label requires a value" >&2; exit 2; }
            LABELS+=("$2")
            shift 2
            ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

run() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] %s\n' "$*"
    else
        "$@"
    fi
}

render_template() {
    sed \
        -e "s|@HOME@|$HOME|g" \
        -e "s|@REPO@|$REPO|g" \
        "$1"
}

label_matches() {
    local unit_basename="$1"
    if [ "${#LABELS[@]}" -eq 0 ]; then
        return 0
    fi
    local l
    for l in "${LABELS[@]}"; do
        if [ "$l" = "$unit_basename" ]; then
            return 0
        fi
    done
    return 1
}

ensure_linger() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] loginctl show-user %s -p Linger --value\n' "$INSTALL_USER"
        if ! command -v loginctl >/dev/null 2>&1; then
            printf '[dry-run] loginctl enable-linger %s # if Linger is not yes (loginctl not on PATH)\n' "$INSTALL_USER"
        else
            printf '[dry-run] loginctl enable-linger %s # if Linger is not yes\n' "$INSTALL_USER"
        fi
        return 0
    fi
    if ! command -v loginctl >/dev/null 2>&1; then
        echo "WARNING: loginctl not found; cannot verify user linger for boot persistence" >&2
        return 0
    fi
    local linger
    linger="$(loginctl show-user "$INSTALL_USER" -p Linger --value 2>/dev/null || true)"
    if [ "$linger" != "yes" ]; then
        if ! loginctl enable-linger "$INSTALL_USER"; then
            echo "WARNING: failed to enable linger for $INSTALL_USER; the user service may not survive logout or reboot" >&2
        fi
    fi
}

# Discover every *.service.template. Bash glob, sorted for determinism.
mapfile -t SERVICE_TEMPLATES < <(cd "$SCRIPT_DIR" && ls -1 *.service.template 2>/dev/null | sort)

if [ "${#SERVICE_TEMPLATES[@]}" -eq 0 ]; then
    echo "No *.service.template files found in $SCRIPT_DIR" >&2
    exit 2
fi

# ---------- --render-only: print the first selected unit and exit ----------
if [ "$RENDER_ONLY" -eq 1 ]; then
    for tmpl in "${SERVICE_TEMPLATES[@]}"; do
        unit_basename="${tmpl%.template}"
        label_matches "$unit_basename" || continue
        render_template "$SCRIPT_DIR/$tmpl"
        exit 0
    done
    echo "No service template matched --label filter" >&2
    exit 2
fi

# ---------- --uninstall: remove every selected unit + sibling timer ----------
if [ "$UNINSTALL" -eq 1 ]; then
    for tmpl in "${SERVICE_TEMPLATES[@]}"; do
        unit_basename="${tmpl%.template}"
        label_matches "$unit_basename" || continue
        # Disable + remove service.
        run systemctl --user disable --now "$unit_basename" || true
        run rm -f "$UNIT_DIR/$unit_basename"
        # Disable + remove sibling timer if present.
        timer_basename="${unit_basename%.service}.timer"
        if [ -f "$SCRIPT_DIR/$timer_basename.template" ]; then
            run systemctl --user disable --now "$timer_basename" || true
            run rm -f "$UNIT_DIR/$timer_basename"
        fi
    done
    run systemctl --user daemon-reload
    echo "Uninstalled dark-factory user units"
    exit 0
fi

# ---------- install path ----------
if [ "$SKIP_BUILD" -eq 0 ]; then
    run cargo build --release --manifest-path "$REPO/daemon/Cargo.toml"
fi
run mkdir -p "$UNIT_DIR" "$LOG_DIR"
ensure_linger

INSTALLED=()
for tmpl in "${SERVICE_TEMPLATES[@]}"; do
    unit_basename="${tmpl%.template}"
    label_matches "$unit_basename" || continue
    # Render + write service unit.
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] render %s > %s/%s\n' "$SCRIPT_DIR/$tmpl" "$UNIT_DIR" "$unit_basename"
    else
        render_template "$SCRIPT_DIR/$tmpl" > "$UNIT_DIR/$unit_basename"
    fi
    INSTALLED+=("$unit_basename")
    # Render + write sibling timer if present.
    timer_basename="${unit_basename%.service}.timer"
    if [ -f "$SCRIPT_DIR/$timer_basename.template" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
            printf '[dry-run] render %s > %s/%s\n' "$SCRIPT_DIR/$timer_basename.template" "$UNIT_DIR" "$timer_basename"
        else
            render_template "$SCRIPT_DIR/$timer_basename.template" > "$UNIT_DIR/$timer_basename"
        fi
        INSTALLED+=("$timer_basename")
    fi
done

run systemctl --user daemon-reload
for unit in "${INSTALLED[@]}"; do
    case "$unit" in
        *.timer)
            run systemctl --user enable --now "$unit"
            ;;
        *.service)
            # Only `enable --now` the daemon service itself; timer-fired
            # oneshot services get fired by their timer on the next
            # OnCalendar slot — enable without --now.
            if [ "$unit" = "ai.dark-factory.daemon.service" ]; then
                run systemctl --user enable --now "$unit"
            else
                run systemctl --user enable "$unit"
            fi
            ;;
    esac
done

for unit in "${INSTALLED[@]}"; do
    if [ "$DRY_RUN" -eq 0 ]; then
        run systemctl --user status "$unit" --no-pager || true
    fi
done

if [ "$DRY_RUN" -eq 1 ]; then
    echo "Dry-run complete; would install: ${INSTALLED[*]}"
else
    echo "Installed dark-factory user units: ${INSTALLED[*]}"
fi