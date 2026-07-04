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
  HOLDOUT_UNSETS=(-u DARK_FACTORY_HOLDOUTS)
  while IFS='=' read -r k _; do case "$k" in *HOLDOUT*) HOLDOUT_UNSETS+=(-u "$k");; esac; done < <(env)
  ( cd "$REPO_DIR" && CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS=0 \
        env "${HOLDOUT_UNSETS[@]}" timeout "$TICK_TIMEOUT" \
        claude -p --dangerously-skip-permissions "$PROMPT" ) >> "$LOG" 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "[$(date -u +%FT%TZ)] tick FAILED (rc=$rc) — continuing to next tick" >> "$LOG"
  fi
  echo "[$(date -u +%FT%TZ)] tick end — sleeping ${INTERVAL}s" >> "$LOG"
  sleep "$INTERVAL"
done
