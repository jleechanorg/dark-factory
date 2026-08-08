#!/usr/bin/env bash
# Idempotent systemd --user installer for the Rust auto-factory daemon.
#
# Usage:
#   daemon/systemd/install-systemd-user.sh [--dry-run] [--uninstall] [--skip-build] [--render-only] [--repo PATH]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CHECKOUT_REPO="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEFAULT_REPO="${DARK_FACTORY_HOME:-$CHECKOUT_REPO}"
INSTALLED_LAUNCHER="${HOME}/.local/bin/dark-factory"
if [ -e "$INSTALLED_LAUNCHER" ] || [ -L "$INSTALLED_LAUNCHER" ]; then
    INSTALLED_REAL="$(readlink -f "$INSTALLED_LAUNCHER" 2>/dev/null || true)"
    if [ -n "$INSTALLED_REAL" ]; then
        INSTALLED_ROOT="$(cd "$(dirname "$INSTALLED_REAL")/.." 2>/dev/null && pwd || true)"
        if [ -n "$INSTALLED_ROOT" ] && [ -f "$INSTALLED_ROOT/.dark-factory-runtime-root" ]; then
            IFS= read -r PINNED_ROOT < "$INSTALLED_ROOT/.dark-factory-runtime-root"
            if [ "$PINNED_ROOT" = "$INSTALLED_ROOT" ]; then
                DEFAULT_REPO="$INSTALLED_ROOT"
            fi
        fi
    fi
fi
REPO="$DEFAULT_REPO"
UNIT_NAME="ai.dark-factory.daemon.service"
UNIT_DIR="${SYSTEMD_USER_DIR:-$HOME/.config/systemd/user}"
UNIT_PATH="$UNIT_DIR/$UNIT_NAME"
LOG_DIR="$HOME/Library/Logs/dark-factory"
INSTALL_USER="${USER:-$(id -un)}"
DRY_RUN=0
UNINSTALL=0
SKIP_BUILD=0
RENDER_ONLY=0

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
        -h|--help)
            sed -n '2,7p' "$0"
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

render_unit() {
    sed \
        -e "s|@HOME@|$HOME|g" \
        -e "s|@REPO@|$REPO|g" \
        "$SCRIPT_DIR/$UNIT_NAME.template"
}

ensure_linger() {
    # Always print the dry-run loginctl commands so the installer's
    # dry-run trace matches the test expectation, even on hosts where
    # loginctl is not installed (e.g. macOS dev hosts). On non-dry-run
    # hosts without loginctl, warn and skip.
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

if [ "$RENDER_ONLY" -eq 1 ]; then
    render_unit
    exit 0
fi

if [ "$UNINSTALL" -eq 1 ]; then
    run systemctl --user disable --now "$UNIT_NAME"
    run rm -f "$UNIT_PATH"
    run systemctl --user daemon-reload
    echo "Uninstalled $UNIT_NAME"
    exit 0
fi

if [ "$SKIP_BUILD" -eq 0 ]; then
    PINNED_REPO=""
    if [ -f "$REPO/.dark-factory-runtime-root" ]; then
        IFS= read -r PINNED_REPO < "$REPO/.dark-factory-runtime-root"
    fi
    if [ "$PINNED_REPO" = "$REPO" ]; then
        [ -x "$REPO/daemon/target/release/daemon" ] || {
            echo "Immutable runtime is missing its prebuilt daemon: $REPO/daemon/target/release/daemon" >&2
            exit 1
        }
    else
        run cargo build --release --manifest-path "$REPO/daemon/Cargo.toml"
    fi
fi
run mkdir -p "$UNIT_DIR" "$LOG_DIR"
ensure_linger
if [ "$DRY_RUN" -eq 1 ]; then
    printf '[dry-run] render %s > %s\n' "$SCRIPT_DIR/$UNIT_NAME.template" "$UNIT_PATH"
else
    render_unit > "$UNIT_PATH"
fi
run systemctl --user daemon-reload
run systemctl --user enable --now "$UNIT_NAME"
run systemctl --user status "$UNIT_NAME" --no-pager
if [ "$DRY_RUN" -eq 1 ]; then
    echo "Dry-run complete for $UNIT_NAME"
else
    echo "Installed $UNIT_NAME"
fi
