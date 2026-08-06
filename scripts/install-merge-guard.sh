#!/usr/bin/env bash
# Idempotent installer for the auto-merge-guard scheduler (jleechan-s3c).
#
# auto-merge-guard.sh already implements the X4 controls (no-red gate,
# head-SHA check, per-hour budget); this script only installs a CALLER:
#   - Linux:   systemd --user unit + timer (60s cadence, Persistent=true)
#   - macOS:   renders the launchd plist template to
#              $HOME/Library/LaunchAgents/ai.dark-factory.auto-merge-guard.plist
#              but does NOT bootstrap it (operator must opt in).
#
# Usage:
#   ./scripts/install-merge-guard.sh [--dry-run] [--uninstall]
#
# Exit codes:
#   0  success
#   2  invalid argument
#   3  systemd user session not available (only on Linux install path)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if [ -d "$SCRIPT_DIR/../daemon" ]; then
    REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
else
    REPO_ROOT="$SCRIPT_DIR"
fi

SYSTEMD_USER_DIR="${SYSTEMD_USER_DIR:-$HOME/.config/systemd/user}"
SERVICE_SRC="$REPO_ROOT/daemon/scripts/systemd-user/dark-factory-merge-guard.service"
TIMER_SRC="$REPO_ROOT/daemon/scripts/systemd-user/dark-factory-merge-guard.timer"
PLIST_TPL_SRC="$REPO_ROOT/daemon/launchd/opt-in/ai.dark-factory.auto-merge-guard.plist.template"
PLIST_TARGET="$HOME/Library/LaunchAgents/ai.dark-factory.auto-merge-guard.plist"
LOG_DIR="$HOME/Library/Logs/dark-factory"
TIMER_NAME="dark-factory-merge-guard.timer"

DRY_RUN=0
UNINSTALL=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --dry-run)   DRY_RUN=1; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help)
            sed -n '2,15p' "$0"
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

os_is_linux() {
    [ "$(uname -s)" = "Linux" ]
}
os_is_macos() {
    [ "$(uname -s)" = "Darwin" ]
}

ensure_log_dir() {
    if [ ! -d "$LOG_DIR" ]; then
        run mkdir -p "$LOG_DIR"
    fi
}

install_systemd_user() {
    # `$USER` is exported by interactive login shells but NOT by CI containers,
    # and this script runs under `set -u`, so a bare "$USER" aborts the whole
    # installer with "line 79: USER: unbound variable" before it can emit its
    # [dry-run] markers. Fall back to the real uid's name, which is always
    # resolvable. Observed on runner ez-runner-c (bead jleechan-kn5j).
    local user_name="${USER:-$(id -un 2>/dev/null || echo unknown)}"
    if ! command -v systemctl >/dev/null 2>&1; then
        echo "[error] systemctl not found on PATH; cannot install systemd user unit" >&2
        exit 3
    fi
    if [ -z "${XDG_RUNTIME_DIR:-}" ] && ! loginctl show-user "$user_name" >/dev/null 2>&1; then
        echo "[error] systemd user session not available (no XDG_RUNTIME_DIR, loginctl inactive)" >&2
        echo "[hint]  enable lingering with: sudo loginctl enable-linger $user_name" >&2
        exit 3
    fi

    ensure_log_dir
    run mkdir -p "$SYSTEMD_USER_DIR"
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] sed -e s%%@REPO@%%%s%%g %s > %s\n' \
            "$REPO_ROOT" "$SERVICE_SRC" "$SYSTEMD_USER_DIR/dark-factory-merge-guard.service"
    else
        sed -e "s%@REPO@%${REPO_ROOT}%g" "$SERVICE_SRC" > "$SYSTEMD_USER_DIR/dark-factory-merge-guard.service"
    fi
    run cp "$TIMER_SRC"   "$SYSTEMD_USER_DIR/dark-factory-merge-guard.timer"

    run systemctl --user daemon-reload
    run systemctl --user enable --now "$TIMER_NAME"
    run systemctl --user status "$TIMER_NAME" --no-pager || true
    echo "Installed systemd user timer: $TIMER_NAME"
}

uninstall_systemd_user() {
    if ! command -v systemctl >/dev/null 2>&1; then
        echo "[skip] systemctl not found; nothing to uninstall"
        return 0
    fi
    run systemctl --user disable --now "$TIMER_NAME" 2>/dev/null || true
    run rm -f "$SYSTEMD_USER_DIR/dark-factory-merge-guard.timer" \
              "$SYSTEMD_USER_DIR/dark-factory-merge-guard.service"
    run systemctl --user daemon-reload
    echo "Uninstalled systemd user timer: $TIMER_NAME"
}

render_macos_plist() {
    # All filesystem mutations route through run() so --dry-run is safe.
    run mkdir -p "$(dirname "$PLIST_TARGET")"
    ensure_log_dir
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] sed -e s%%@HOME@%%%s%%g -e s%%@REPO@%%%s%%g %s > %s\n' \
            "$HOME" "$REPO_ROOT" "$PLIST_TPL_SRC" "$PLIST_TARGET"
    else
        sed -e "s%@HOME@%${HOME}%g" -e "s%@REPO@%${REPO_ROOT}%g" \
            "$PLIST_TPL_SRC" > "$PLIST_TARGET"
        chmod 0644 "$PLIST_TARGET"
    fi
    echo "Rendered macOS plist (NOT bootstrapped — operator opt-in): $PLIST_TARGET"
    echo "  To activate: launchctl bootstrap gui/\$UID \"$PLIST_TARGET\""
}

uninstall_macos_plist() {
    if [ -f "$PLIST_TARGET" ]; then
        run launchctl bootout "gui/$UID" "$PLIST_TARGET" 2>/dev/null || true
        run rm -f "$PLIST_TARGET"
        echo "Removed $PLIST_TARGET"
    else
        echo "[skip] $PLIST_TARGET not present"
    fi
}

if [ "$UNINSTALL" -eq 1 ]; then
    if os_is_linux; then
        uninstall_systemd_user
    fi
    if os_is_macos; then
        uninstall_macos_plist
    fi
    exit 0
fi

if os_is_linux; then
    install_systemd_user
fi
if os_is_macos; then
    render_macos_plist
fi

if ! os_is_linux && ! os_is_macos; then
    echo "[warn] unsupported OS ($(uname -s)); nothing to install" >&2
    exit 0
fi

if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] complete"
fi