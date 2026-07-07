#!/usr/bin/env bash
# Spawn AO remediation for a factory ATTESTED bead — isolated target-repo worktree.
# Uses Go AO mirror (~/bin/ao-go) by default; TS fallback via AO_BIN=~/bin/ao-ts.
#
# Async contract (default; fixes AF tick blocking on cold-start)
# -------------------------------------------------------------
# factory-af-tick.sh runs every 240s via launchd. If this script blocked on
# the AO spawn (up to AO_SPAWN_TIMEOUT_SEC=120s), the tick loop would run late
# and back up. Worse: on cold-start the AO daemon is not yet running, so the
# spawn blocks for the FULL timeout.
#
# Default behavior (ASYNC=1):
#   - Pre-flight probe (≤5s wallclock): ensure AO daemon is reachable; if not,
#     kick the Go daemon with a bounded retry loop. Fail loud if unreachable.
#   - Detach the real spawn into a background process. Return 0 immediately
#     with an "[remediate] async-spawned" message that includes pid + log path
#     so the AF tick can record the dispatch state without waiting.
#   - The background process writes its result to a state file so the NEXT
#     tick can detect failures via the existing `ao session ls` check.
#
# Sync behavior (SYNC=1):
#   - Preserves the original blocking behavior for tests and manual callers.
#   - Used by tests/scripts/test_factory_ao_remediate.sh to assert that the
#     sync path still works as before.
#
# Env vars:
#   SYNC=1                    opt into blocking behavior (tests / manual)
#   ASYNC=0                   same as SYNC=1 (explicit)
#   AO_SPAWN_TIMEOUT_SEC      seconds before spawn times out (default 120)
#   AFD_LOG_DIR               directory for spawn log files (default
#                             $HOME/Library/Logs/dark-factory)
#   AFD_SPAWN_STATE_DIR       directory for state files (default
#                             $HOME/Library/Application Support/dark-factory/spawns)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export AO_MAX_CONCURRENT_SESSIONS="${AO_MAX_CONCURRENT_SESSIONS:-30}"
AO="$(bash "$ROOT/daemon/factory-ao-bin.sh")"
BEAD_ID="${1:?bead_id required}"
PR="${2:?pr_number required}"
TARGET_REPO="${3:-jleechanorg/worldarchitect.ai}"
AO_PROJECT="${4:-worldarchitect}"
SPAWN_TIMEOUT="${AO_SPAWN_TIMEOUT_SEC:-120}"
DISPLAY_NAME="$(python3 -c 'import sys; print(sys.argv[1][:20])' "$BEAD_ID")"
LOG_DIR="${AFD_LOG_DIR:-$HOME/Library/Logs/dark-factory}"
STATE_DIR="${AFD_SPAWN_STATE_DIR:-$HOME/Library/Application Support/dark-factory/spawns}"
SPAWN_LOG="$LOG_DIR/remediate-${BEAD_ID}-$(date -u +%Y%m%dT%H%M%SZ).log"
STATE_FILE="$STATE_DIR/${BEAD_ID}-${PR}.state"

# Mode resolution: SYNC=1 OR ASYNC=0 → sync; otherwise async (default).
if [ "${SYNC:-0}" = "1" ] || [ "${ASYNC:-1}" = "0" ]; then
  MODE="sync"
else
  MODE="async"
fi

PROMPT="Factory bead ${BEAD_ID}: drive PR #${PR} on ${TARGET_REPO} to /green + /er. Push to existing branch only; do NOT open new PR; do NOT merge."

# Pre-flight: ensure AO is reachable. Bounded at 5s wallclock so the
# async path never blocks more than that on cold-start. Two failure modes
# are caught:
#   1. ao-go daemon not running → start it with a bounded retry loop.
#   2. AO binary itself broken / not executable → fail loud immediately
#      so the tick can skip the bead instead of silently queueing a doomed
#      spawn that will never produce an `ao session ls` row.
ensure_ao_daemon() {
  # Catch broken AO binary first: a 127 exit on any command means the
  # CLI is misconfigured (wrong path, missing exec bit, broken install).
  # Don't queue a doomed spawn in that case.
  if ! "$AO" --version >/dev/null 2>&1 && ! "$AO" status >/dev/null 2>&1; then
    return 1
  fi
  if [[ "$(basename "$AO")" != "ao-go" ]]; then
    # ao-ts manages its own lifecycle; binary is the daemon.
    return 0
  fi
  if "$AO" status >/dev/null 2>&1; then
    return 0
  fi
  echo "[remediate] starting Go AO daemon" >&2
  nohup "$AO" daemon >> /tmp/ao-go-daemon.log 2>&1 &
  for _ in 1 2 3 4 5; do
    if "$AO" status >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  return 1
}

run_spawn_foreground() {
  # Returns "exit_code<TAB>output" so the caller can branch on rc.
  local out rc
  set +e
  if "$AO" spawn --help 2>&1 | rg -q '\-\-name'; then
    out="$(timeout "$SPAWN_TIMEOUT" "$AO" spawn --project "$AO_PROJECT" --name "$DISPLAY_NAME" --agent claude-code --claim-pr "$PR" --prompt "$PROMPT" 2>&1)"
  else
    out="$(timeout "$SPAWN_TIMEOUT" "$AO" spawn --project "$AO_PROJECT" --claim-pr "$PR" --agent claude-code "$PROMPT" 2>&1)"
  fi
  rc=$?
  set -e
  printf '%s\t%s\n' "$rc" "$out"
}

classify_spawn_outcome() {
  # $1 = spawn rc, $2 = spawn output. Echoes 0 (success) or 1 (failure).
  local rc="$1" out="$2"
  if [ "$rc" -eq 0 ]; then return 0; fi
  if echo "$out" | rg -q 'spawned session |Session [a-z0-9_-]+ created|✓ Session|pr_open|working|spawning|claimed https://'; then
    return 0
  fi
  if "$AO" session ls 2>/dev/null | rg "pulls/${PR}\b" | rg -q "\[(spawning|running|active|working|pr_open)\]"; then
    return 0
  fi
  return 1
}

# ---------- SYNC PATH (original blocking behavior) ----------
if [ "$MODE" = "sync" ]; then
  MINIMAX_SYNC="$ROOT/daemon/factory-ao-minimax-sync.sh"
  if [ -x "$MINIMAX_SYNC" ]; then
    bash "$MINIMAX_SYNC" --all || echo "[remediate] WARN: MiniMax sync failed — sessions may use Anthropic OAuth" >&2
  fi
  # Best-effort daemon readiness for sync path (no bounded probe — caller
  # is opting into blocking semantics).
  if [[ "$(basename "$AO")" == "ao-go" ]]; then
    state="$("$AO" status --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state",""))' 2>/dev/null || true)"
    if [ "$state" != "ready" ] && [ "$state" != "running" ]; then
      echo "[remediate] starting Go AO daemon" >&2
      nohup "$AO" daemon >> /tmp/ao-go-daemon.log 2>&1 &
      sleep 2
    fi
  fi
  result="$(run_spawn_foreground)"
  rc="${result%%$'\t'*}"
  out="${result#*$'\t'}"
  if classify_spawn_outcome "$rc" "$out" >/dev/null; then
    [ "$rc" -eq 0 ] || echo "[remediate] spawn accepted for PR #$PR (timeout=${SPAWN_TIMEOUT}s, rc=$rc)" >&2
    echo "$out"
    exit 0
  fi
  echo "$out" >&2
  exit 1
fi

# ---------- ASYNC PATH (default; non-blocking) ----------
# Pre-flight: bounded 5s probe. Fail loud if AO is unreachable so the tick
# can skip the bead instead of silently queueing a doomed spawn.
if ! ensure_ao_daemon; then
  echo "[remediate] AO unreachable after 5s probe — refusing to async-spawn" >&2
  exit 1
fi

mkdir -p "$LOG_DIR" "$STATE_DIR"
# Mark "pending" so a stale state file from a previous crashed run doesn't
# cause the next tick to misread it. Background process overwrites with final.
echo "pending" > "$STATE_FILE"

# Detach the real spawn. Background process records outcome to STATE_FILE.
(
  set +e
  result="$(run_spawn_foreground)"
  rc="${result%%$'\t'*}"
  out="${result#*$'\t'}"
  printf '%s' "$out" > "$SPAWN_LOG"
  if classify_spawn_outcome "$rc" "$out" >/dev/null; then
    echo "ok" > "$STATE_FILE"
  else
    echo "fail:rc=$rc" > "$STATE_FILE"
  fi
) >/dev/null 2>&1 &
SPAWN_PID=$!
disown "$SPAWN_PID" 2>/dev/null || true

echo "[remediate] async-spawned PR #$PR bead=${BEAD_ID} pid=${SPAWN_PID} log=${SPAWN_LOG}"
# Caller (factory-af-tick.sh) treats rc=0 as "dispatch accepted". The
# `ao session ls` check on subsequent ticks will skip this PR if the spawn
# never produced a session — same retry semantics as before.
exit 0