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
mkdir -p "$LOG_DIR"

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
  # per-tick timeout = 90% of interval or 20m, whichever is larger
  TICK_TIMEOUT=$(( INTERVAL * 9 / 10 > 1200 ? INTERVAL * 9 / 10 : 1200 ))
  if ! ( cd "$REPO_DIR" && timeout "$TICK_TIMEOUT" \
        claude -p --dangerously-skip-permissions "$PROMPT" ) >> "$LOG" 2>&1; then
    echo "[$(date -u +%FT%TZ)] tick FAILED (rc=$?) — continuing to next tick" >> "$LOG"
  fi
  echo "[$(date -u +%FT%TZ)] tick end — sleeping ${INTERVAL}s" >> "$LOG"
  sleep "$INTERVAL"
done
