#!/usr/bin/env bash
# Idempotent installer for dark-factory launchd agents.
#
# Reads every *.plist.template in this directory, substitutes @HOME@ with
# $HOME, and copies the result to $HOME/Library/LaunchAgents/<basename>.plist.
# Supports --dry-run (echo actions without executing) and --uninstall
# (remove installed plists + bootout agents).
#
# Usage:
#   ./install-launchagents.sh [--dry-run] [--uninstall]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LAUNCH_AGENTS_DIR="$HOME/Library/LaunchAgents"
LOG_DIR="$HOME/Library/Logs/dark-factory"
LAUNCHCTL_DOMAIN="gui/$UID"

DRY_RUN=0
UNINSTALL=0
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help)
            echo "Usage: $0 [--dry-run] [--uninstall]"
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
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

ensure_log_dir() {
    if [ ! -d "$LOG_DIR" ]; then
        run mkdir -p "$LOG_DIR"
    fi
}

discover_plists() {
    # Emit one "<label>|<template>" line per .plist.template, label derived
    # from filename (foo.plist.template -> foo).
    find "$SCRIPT_DIR" -maxdepth 1 -type f -name '*.plist.template' \
        | while read -r tpl; do
            base="$(basename "$tpl" .plist.template)"
            echo "${base}|${tpl}"
        done
}

install_one() {
    local label="$1"
    local tpl="$2"
    local target="$LAUNCH_AGENTS_DIR/${label}.plist"

    ensure_log_dir

    # Render template -> target via sed.
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] sed s|@HOME@|%s|g %s > %s\n' "$HOME" "$tpl" "$target"
    else
        sed "s|@HOME@|${HOME}|g" "$tpl" > "$target"
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
        launchctl bootstrap "$LAUNCHCTL_DOMAIN" "$target"
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

mkdir -p "$LAUNCH_AGENTS_DIR" 2>/dev/null || run mkdir -p "$LAUNCH_AGENTS_DIR"

while IFS='|' read -r label tpl; do
    [ -n "$label" ] || continue
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