#!/usr/bin/env bash
# daemon/qw5-pilot-dispatch.sh
#
# One-shot dispatch script for the jleechan-qw5 N-shadow reviewer fan-out pilot.
# Invoked by the launchd job at daemon/launchd/local.dark-factory.qw5-pilot-dispatch.plist
# ~2h from when the plist is bootstrapped.
#
# Behavior:
#   1. Check sentinel; if already dispatched, self-unload and exit (idempotent).
#   2. git fetch origin/main; create or reuse the worktree at /tmp/qw5-coder-wt.
#   3. Set minimax env vars (binding — backend constraint).
#   4. Spawn the coder subagent via `claudem -p <prompt>` and stream its log.
#   5. On completion, write a CXDB pointer + report tail.
#   6. Self-unload the launchd job so it doesn't re-fire.
#
# Backend constraint (binding per user directive 2026-07-04):
#   - minimax-only. NO claude sonnet/opus/fable, NO agy (Antigravity CLI), NO agentf/cursor.
#
# This script is REPO-OWNED (lives at the dark-factory repo root). The active
# launchd plist is a thin wrapper that calls this script.

set -euo pipefail

BEAD_ID="jleechan-qw5"
REPO="/Users/jleechan/projects/dark-factory"
BRANCH="feat/qw5-n-shadow-fanout"
WT="/tmp/qw5-coder-wt"
SENTINEL="/tmp/${BEAD_ID}-dispatch.done"
PLIST_LABEL="local.dark-factory.qw5-pilot-dispatch"
PLIST_SRC="${REPO}/daemon/launchd/${PLIST_LABEL}.plist.template"
PLIST_DST="${HOME}/Library/LaunchAgents/${PLIST_LABEL}.plist"
LOGDIR="/tmp"
OUT="${LOGDIR}/${BEAD_ID}-dispatch.stdout.log"
ERR="${LOGDIR}/${BEAD_ID}-dispatch.stderr.log"

mkdir -p "$(dirname "$OUT")"

log() { printf '[%s] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*"; }
exec >>"$OUT" 2>>"$ERR"

log "===== ${BEAD_ID} dispatch start ====="

# Sourcing profiles to load user environment in launchd context
for profile in "${HOME}/.bash_profile" "${HOME}/.bashrc" "${HOME}/.zshrc"; do
  if [ -f "$profile" ]; then
    source "$profile" 2>/dev/null || true
  fi
done

# Ensure typical user paths are on PATH
export PATH="/Users/jleechan/.local/bin:${HOME}/.local/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:$PATH"

# Sanity-check the minimax wrapper is on PATH.
if ! command -v claudem >/dev/null; then
  log "FATAL: claudem not on PATH (checked after path expansions)"
  exit 64
fi

if [ ! -f "${REPO}/daemon/qw5-coder-prompt.md" ]; then
  log "FATAL: coder prompt file missing at ${REPO}/daemon/qw5-coder-prompt.md"
  exit 65
fi

# 1. Idempotency sentinel — self-unload and exit if already fired.
if [ -f "$SENTINEL" ]; then
  log "sentinel ${SENTINEL} already present; self-unloading and exiting (no-op)"
  launchctl bootout "gui/$(id -u)" "$PLIST_DST" 2>/dev/null || true
  exit 0
fi
touch "$SENTINEL"
log "sentinel created at ${SENTINEL}"

# 2. Fetch + ensure worktree.
cd "$REPO"
git fetch origin main

if ! git worktree list --porcelain | grep -q "^worktree ${WT}"; then
  log "creating worktree at ${WT} on branch ${BRANCH} off origin/main"
  git worktree add "$WT" -b "$BRANCH" origin/main
else
  log "worktree ${WT} already exists; reusing"
  cd "$WT"
  git rebase origin/main || log "rebase failed (continuing with current state)"
fi

# 3. Backend env vars — binding per user directive (minimax ONLY).
#
# launchd inherits the operator's environment, and the sourced profiles may
# add more provider state. Scrub every Claude/Anthropic routing variable and
# every MiniMax routing variable except the credential, then set only the
# pinned MiniMax endpoint/model/key values used by the wrapper.
configure_minimax_env() {
  local minimax_key="${MINIMAX_API_KEY:-}"
  local var
  for var in $(compgen -A variable); do
    case "$var" in
      CLAUDE_*|CLAUDEM_MODE|DARK_FACTORY_CLAUDE_CONFIG_DIR|ANTHROPIC_*) unset "$var" ;;
      MINIMAX_*|DARK_FACTORY_MINIMAX_MODEL) [[ "$var" == "MINIMAX_API_KEY" ]] || unset "$var" ;;
    esac
  done
  export MINIMAX_API_KEY="$minimax_key"
  export ANTHROPIC_BASE_URL="https://api.minimax.io/anthropic"
  export ANTHROPIC_AUTH_TOKEN="$MINIMAX_API_KEY"
  export ANTHROPIC_API_KEY="$MINIMAX_API_KEY"
  export ANTHROPIC_MODEL="MiniMax-M3"
  export ANTHROPIC_SMALL_FAST_MODEL="MiniMax-M3"
  export CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL=0
}

configure_minimax_env
log "minimax env vars set; ANTHROPIC_MODEL=${ANTHROPIC_MODEL}"

cd "$WT"

log "dispatching coder via claudem -p \$(cat ${REPO}/daemon/qw5-coder-prompt.md)"
# Run with stdin closed to avoid interactive prompts in launchd context.
claudem -p "$(cat "${REPO}/daemon/qw5-coder-prompt.md")" \
  </dev/null \
  || log "claudem exited non-zero (continuing; coder log in claude session output)"

# 6. Self-unload so we never re-fire.
log "self-unloading launchd job ${PLIST_LABEL}"
launchctl bootout "gui/$(id -u)" "$PLIST_DST" 2>/dev/null || true

log "===== ${BEAD_ID} dispatch end ====="
