#!/usr/bin/env bash
# deploy-af-tick.sh — explicit, audited deploy step for the ai.dark-factory.af-tick
# launchd daemon's execution checkout.
#
# Bead jleechan-vxs8: the launchd daemon executes whatever branch happens to
# be checked out in its execution root (default ~/projects/dark-factory, a
# dev working tree also used interactively). Before this bead, "deploying" a
# change to the daemon meant `git checkout <branch>` in that tree — an
# unaudited, undocumented action indistinguishable from a dev poking around.
# On 2026-07-11 the tree sat on a crashing feature branch for hours, then was
# silently switched to a different branch by another session; neither state
# was a deliberate deploy.
#
# This script is the ONE sanctioned way to move the daemon's checkout
# forward. It:
#   1. Refuses to run if the deploy target has uncommitted changes (dirty).
#   2. Refuses to run if the deploy target is not on `main`.
#   3. Fetches origin, fast-forwards main to origin/main.
#   4. Logs the SHA transition (old -> new) to stdout AND to a JSONL audit
#      log under ~/Library/Logs/dark-factory/deploy.jsonl so every deploy is
#      forensically reconstructible.
#   5. Is a no-op (rc=0, logs "no-op") when old == new — repeated runs are
#      safe and cheap (idempotent), matching install-launchagents.sh style.
#
# This does NOT restart the launchd daemon (single-writer rule — restarts
# are the deploy-owner's job, not this script's; see daemon/launchd/README.md
# and CLAUDE.md "single-writer" policy). It only fast-forwards the checkout;
# the running af-tick.sh process picks up the new code on its next
# invocation (each tick execs a fresh script read from disk).
#
# Usage:
#   daemon/scripts/deploy-af-tick.sh [--target-dir <path>] [--dry-run]
#
# Environment:
#   AFD_DEPLOY_TARGET_DIR   overrides the default deploy target
#                           (default: $HOME/projects/dark-factory)
#   AFD_DEPLOY_LOG          overrides the JSONL audit log path
#                           (default: $HOME/Library/Logs/dark-factory/deploy.jsonl)
#
# Exit codes:
#   0  success (deployed or already up to date)
#   2  invalid argument
#   3  target directory missing / not a git repo
#   4  target directory is dirty (uncommitted changes) — refusing
#   5  target directory is not on main — refusing
#   6  fetch or fast-forward failed
set -euo pipefail

TARGET_DIR="${AFD_DEPLOY_TARGET_DIR:-$HOME/projects/dark-factory}"
DEPLOY_LOG="${AFD_DEPLOY_LOG:-$HOME/Library/Logs/dark-factory/deploy.jsonl}"
DRY_RUN=0

i=1
while [ "$i" -le "$#" ]; do
    arg="${@:$i:1}"
    case "$arg" in
        --target-dir)
            i=$((i + 1))
            if [ "$i" -gt "$#" ]; then
                echo "deploy-af-tick: --target-dir requires a value" >&2
                exit 2
            fi
            TARGET_DIR="${@:$i:1}"
            ;;
        --dry-run) DRY_RUN=1 ;;
        -h|--help)
            cat <<EOF
Usage: $0 [--target-dir <path>] [--dry-run]

Fast-forwards the dark-factory daemon's execution checkout (default:
\$HOME/projects/dark-factory) to origin/main, refusing if the checkout is
dirty or not on main. Logs the SHA transition to stdout and to
$DEPLOY_LOG. Does NOT restart the daemon.
EOF
            exit 0
            ;;
        *) echo "deploy-af-tick: unknown argument: $arg" >&2; exit 2 ;;
    esac
    i=$((i + 1))
done

if [ ! -d "$TARGET_DIR/.git" ]; then
    echo "deploy-af-tick: $TARGET_DIR is not a git repo (missing .git) — refusing" >&2
    exit 3
fi

cd "$TARGET_DIR"

current_branch="$(git branch --show-current 2>/dev/null || true)"
if [ "$current_branch" != "main" ]; then
    echo "deploy-af-tick: $TARGET_DIR is on branch '${current_branch:-<detached HEAD>}', not main — refusing to deploy. Switch to main manually first (this script will not do it for you, to avoid silently discarding whatever a human/session put there deliberately)." >&2
    exit 5
fi

if ! git diff --quiet HEAD -- 2>/dev/null || ! git diff --quiet --cached HEAD -- 2>/dev/null; then
    echo "deploy-af-tick: $TARGET_DIR has uncommitted changes — refusing to deploy dirty source. Run 'git status' in $TARGET_DIR and clean the tree first." >&2
    exit 4
fi

old_sha="$(git rev-parse HEAD)"

if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] would: git fetch origin main && git merge --ff-only origin/main"
    echo "[dry-run] current HEAD: ${old_sha:0:9}"
    exit 0
fi

if ! git fetch origin main --quiet; then
    echo "deploy-af-tick: git fetch origin main failed" >&2
    exit 6
fi

if ! git merge --ff-only origin/main --quiet 2>/dev/null; then
    echo "deploy-af-tick: fast-forward to origin/main failed (local main has diverged commits not on origin/main — this should not happen on a deploy-only checkout; investigate manually, do not force)" >&2
    exit 6
fi

new_sha="$(git rev-parse HEAD)"

mkdir -p "$(dirname "$DEPLOY_LOG")"
deploy_ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

if [ "$old_sha" = "$new_sha" ]; then
    echo "deploy-af-tick: no-op — $TARGET_DIR already at origin/main (${new_sha:0:9})"
    printf '{"ts":"%s","target_dir":"%s","old_sha":"%s","new_sha":"%s","noop":true}\n' \
        "$deploy_ts" "$TARGET_DIR" "$old_sha" "$new_sha" >> "$DEPLOY_LOG"
    exit 0
fi

echo "deploy-af-tick: DEPLOYED $TARGET_DIR: ${old_sha:0:9} -> ${new_sha:0:9}"
printf '{"ts":"%s","target_dir":"%s","old_sha":"%s","new_sha":"%s","noop":false}\n' \
    "$deploy_ts" "$TARGET_DIR" "$old_sha" "$new_sha" >> "$DEPLOY_LOG"

echo
echo "NOTE: the running ai.dark-factory.af-tick launchd job will pick up this"
echo "code on its NEXT tick (each tick execs factory-af-tick.sh fresh from"
echo "disk) — no restart required. If you need to force an immediate pickup,"
echo "that is a separate, deliberate action outside this script's scope"
echo "(single-writer rule: only the designated deploy-owner restarts the"
echo "live launchd job, and never as a side effect of a deploy script)."
