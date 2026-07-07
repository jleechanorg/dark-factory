#!/usr/bin/env bash
# launchd-wrapper.sh — bridge script for dark-factory launchd agents.
#
# launchd runs agents with a minimal PATH (/usr/bin:/bin:/usr/sbin:/sbin) and
# no sourced shell init. dark-factory tick scripts depend on homebrew/conda
# tooling (br, gh, sqlite3, python3, callpath, jq, etc.) and on the user's
# git/SSH configuration. This wrapper sources the user's interactive login
# environment (with set +u / -u guards around the source so a strict-mode
# bashrc doesn't break us) before exec'ing the target script.
#
# Usage: launchd-wrapper.sh /absolute/path/to/target.sh [args...]
#
# Conventions:
#   - First arg is the absolute path of the script to run.
#   - All subsequent args are forwarded.
#   - Exit code is the target script's exit code.

set -e

if [ "$#" -lt 1 ]; then
    echo "Usage: $0 /absolute/path/to/target.sh [args...]" >&2
    exit 64
fi

TARGET="$1"
shift

if [ ! -x "$TARGET" ]; then
    echo "[launchd-wrapper] target not executable: $TARGET" >&2
    exit 66
fi

# Source login shell profile to get PATH, git config, SSH agent env, etc.
# set +u / -u guards are required because bashrc often uses unbound vars.
if [ -f "$HOME/.bash_profile" ]; then
    set +u
    # shellcheck disable=SC1090
    source "$HOME/.bash_profile"
    set -u
elif [ -f "$HOME/.profile" ]; then
    set +u
    # shellcheck disable=SC1090
    source "$HOME/.profile"
    set -u
elif [ -f "$HOME/.bashrc" ]; then
    set +u
    # shellcheck disable=SC1090
    source "$HOME/.bashrc"
    set -u
fi

# Make sure the launchd-injected PATH at least contains the usual homebrew dirs.
case ":$PATH:" in
    *":/opt/homebrew/bin:"*) ;;
    *":/usr/local/bin:"*) ;;
    *)
        export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
        ;;
esac

# Forward to the actual target.
exec "$TARGET" "$@"