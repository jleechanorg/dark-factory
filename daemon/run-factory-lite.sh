#!/usr/bin/env bash
# factory-lite background loop runner — isolated headless processes, not in-session subagents.
# Usage: ./daemon/run-factory-lite.sh coder|verifier [interval_secs] [max_wall_secs]
#   coder    default interval 600s  (10m)
#   verifier default interval 300s  (5m)
# Both default max_wall_secs=14400 (4h hard stop, matches operator time-box).
# Logs: ~/Library/Logs/dark-factory/factory-lite-<role>.log
set -euo pipefail

ROLE="${1:?role required: coder|verifier}"
case "$ROLE" in
  coder)    DEFAULT_INTERVAL=600 ;;
  verifier) DEFAULT_INTERVAL=300 ;;
  *) echo "unknown role: $ROLE (want coder|verifier)" >&2; exit 2 ;;
esac
INTERVAL="${2:-$DEFAULT_INTERVAL}"
MAX_WALL="${3:-14400}"

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOG_DIR="$HOME/Library/Logs/dark-factory"
LOG="$LOG_DIR/factory-lite-$ROLE.log"
mkdir -p "$LOG_DIR" "$HOME/.dark-factory"

# single-instance guard: exactly one loop per role (prevents capacity TOCTOU races)
# FLOCK_BLOCK=1: wait for the lock instead of failing (graceful takeover from a retiring loop)
exec 9>"$HOME/.dark-factory/factory-lite-$ROLE.lock"
if [ "${FLOCK_BLOCK:-0}" = "1" ]; then
  echo "[$(date -u +%FT%TZ)] waiting for factory-lite-$ROLE lock (takeover mode)..."
  flock 9
else
  if ! flock -n 9; then
    echo "another factory-lite-$ROLE loop is already running — refusing to start" >&2
    exit 3
  fi
fi

PROMPT="Invoke the factory-lite-$ROLE skill with the Skill tool and execute exactly one tick, then stop. Follow the skill and .claude/skills/factory-lite/CONTRACT.md exactly — especially every NEVER rule."

START=$(date +%s)
echo "[$(date -u +%FT%TZ)] factory-lite-$ROLE loop start pid=$$ interval=${INTERVAL}s max_wall=${MAX_WALL}s" >> "$LOG"

# Backend fallback chain (user directive 2026-07-05: anthropic primary, agy fallback, minimax last).
# Tries each CLI in order; on quota/CLI-not-found error, falls back to next. Real dispatch, not
# decorative metadata (per jleechan-pqip priority-queue-dispatch invariant).
# Override priority via FACTORY_BACKEND env var (e.g. FACTORY_BACKEND_ORDER="agy claude claudem"
# forces agy-first).
invoke_with_fallback() {
  local prompt="$1" timeout_secs="$2"
  local order="${FACTORY_BACKEND_ORDER:-claude agy claudem}"
  for backend in $order; do
    case "$backend" in
      claude)
        if ! command -v claude >/dev/null 2>&1; then continue; fi
        echo "[$(date -u +%FT%TZ)] backend=claude (primary)" >> "$LOG"
        if timeout "$timeout_secs" claude -p --dangerously-skip-permissions "$prompt" 2>>"$LOG"; then
          echo "[$(date -u +%FT%TZ)] backend=claude OK" >> "$LOG"; return 0
        fi
        echo "[$(date -u +%FT%TZ)] backend=claude FAILED (rc=$?) — falling back" >> "$LOG"
        ;;
      agy)
        if ! command -v agy >/dev/null 2>&1; then continue; fi
        echo "[$(date -u +%FT%TZ)] backend=agy (fallback)" >> "$LOG"
        if timeout "$timeout_secs" agy --print --dangerously-skip-permissions "$prompt" 2>>"$LOG"; then
          echo "[$(date -u +%FT%TZ)] backend=agy OK" >> "$LOG"; return 0
        fi
        echo "[$(date -u +%FT%TZ)] backend=agy FAILED (rc=$?) — falling back" >> "$LOG"
        ;;
      claudem|minimax)
        local cli="claudem"
        if ! command -v "$cli" >/dev/null 2>&1; then continue; fi
        echo "[$(date -u +%FT%TZ)] backend=$cli (last resort)" >> "$LOG"
        if timeout "$timeout_secs" "$cli" -p --dangerously-skip-permissions "$prompt" 2>>"$LOG"; then
          echo "[$(date -u +%FT%TZ)] backend=$cli OK" >> "$LOG"; return 0
        fi
        echo "[$(date -u +%FT%TZ)] backend=$cli FAILED (rc=$?) — exhausted" >> "$LOG"
        ;;
    esac
  done
  return 1
}

while :; do
  NOW=$(date +%s)
  if (( NOW - START >= MAX_WALL )); then
    echo "[$(date -u +%FT%TZ)] max_wall ${MAX_WALL}s reached — exiting" >> "$LOG"
    exit 0
  fi
  echo "[$(date -u +%FT%TZ)] tick begin" >> "$LOG"
  # per-tick timeout = 90% of interval or 45m, whichever is larger — a tick now
  # waits for its background coders (see env below), so it must outlive the
  # longest coder; a long tick simply delays the next one (sleep runs after)
  TICK_TIMEOUT=$(( INTERVAL * 9 / 10 > 2700 ? INTERVAL * 9 / 10 : 2700 ))
  rc=0
  # CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0: wait for background coder subagents
  # instead of killing them at 600s (f-signoff P2 — confirmed killing Task-10's
  # coder 2026-07-04T17:48Z). TICK_TIMEOUT still bounds the whole tick.
  # Holdout isolation: strip DARK_FACTORY_HOLDOUTS and any *HOLDOUT* var from the
  # tick (and thus its spawned coder subagents), mirroring the Python runner's
  # _sanitized_env — coder/implementing agents must never see holdout paths.
  # invoke_with_fallback is a SHELL FUNCTION in this script (not an executable) —
  # we call it directly and unset holdout vars inline (env cmd can't see shell funcs).
  ( cd "$REPO_DIR" && CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0 \
      unset DARK_FACTORY_HOLDOUTS; \
      while IFS='=' read -r k _; do case "$k" in *HOLDOUT*) unset "$k";; esac; done < <(env -0); \
      invoke_with_fallback "$PROMPT" "$TICK_TIMEOUT" ) >> "$LOG" 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "[$(date -u +%FT%TZ)] tick FAILED (rc=$rc) — continuing to next tick" >> "$LOG"
  fi
  echo "[$(date -u +%FT%TZ)] tick end — sleeping ${INTERVAL}s" >> "$LOG"
  sleep "$INTERVAL"
done
