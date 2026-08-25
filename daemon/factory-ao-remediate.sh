#!/usr/bin/env bash
# Bead jleechan-kn5j: uses `grep -E`, not ripgrep. `rg` is NOT installed on the
# CI runners ("factory-ao-remediate.sh: line 141: rg: command not found"), and
# this script is exercised by tests/scripts/test_factory_ao_remediate.sh. The
# patterns here are plain alternations, so grep -E is a faithful substitute and
# removes a hard dependency on a tool the runner image does not guarantee.
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
#     with an "[remediate] async-spawned" message that includes pid + log path.
#   - With AFD_REQUIRE_SESSION=1, the background process writes `ok` only after
#     the exact project-scoped AO session is visible. The next AF tick then
#     reconciles that session and records DISPATCHED without blocking here.
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
#   AFD_ASYNC_WAIT_SEC        how long the async wrapper polls the state
#                             file for fast-fail detection before returning
#                             optimistically (default 5). Most auth/project
#                             errors fail within 1-2s; cold-start slow spawns
#                             exceed this bound and proceed optimistically.
#   AFD_REQUIRE_SESSION=1     sync callers require a visible, active
#                             project-scoped AO session before accepting spawn
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export AO_MAX_CONCURRENT_SESSIONS="${AO_MAX_CONCURRENT_SESSIONS:-30}"
AO="$(bash "$ROOT/daemon/factory-ao-bin.sh")"
BEAD_ID="${1:?bead_id required}"
PR="${2:?pr_number required}"
TARGET_REPO="${3:-jleechanorg/worldarchitect.ai}"
AO_PROJECT="${4:-worldarchitect}"
DISPATCH_NONCE="${5:-}"
[ -z "$DISPATCH_NONCE" ] || [[ "$DISPATCH_NONCE" =~ ^[A-Za-z0-9._-]+$ ]] \
  || { echo "[remediate] invalid dispatch nonce" >&2; exit 2; }
SPAWN_TIMEOUT="${AO_SPAWN_TIMEOUT_SEC:-120}"
READY_TIMEOUT="${AFD_AO_READY_TIMEOUT_SEC:-5}"
case "$READY_TIMEOUT" in ''|*[!0-9]*|0) READY_TIMEOUT=5 ;; esac
READY_DEADLINE=0
DISPLAY_NAME="$(python3 -c 'import sys; print(sys.argv[1][:20])' "$BEAD_ID")"
LOG_DIR="${AFD_LOG_DIR:-$HOME/Library/Logs/dark-factory}"
STATE_DIR="${AFD_SPAWN_STATE_DIR:-$HOME/Library/Application Support/dark-factory/spawns}"
SPAWN_LOG="$LOG_DIR/remediate-${BEAD_ID}-$(date -u +%Y%m%dT%H%M%SZ).log"
STATE_FILE="$STATE_DIR/${BEAD_ID}-${PR}${DISPATCH_NONCE:+-$DISPATCH_NONCE}.state"

# Mode resolution: SYNC=1 OR ASYNC=0 → sync; otherwise async (default).
if [ "${SYNC:-0}" = "1" ] || [ "${ASYNC:-1}" = "0" ]; then
  MODE="sync"
else
  MODE="async"
fi

# Pull bead body so the worker sees the goal artifact, not just IDs.
# Use `description` (which inlines the "Acceptance:" paragraph) + the
# dedicated `acceptance_criteria` field if populated.
BEAD_JSON="$("$ROOT/../bin/br" --db "${BR_DB:-$ROOT/../.beads/beads.db}" show "$BEAD_ID" --json 2>/dev/null || true)"
if [ -z "$BEAD_JSON" ]; then
  if command -v br >/dev/null 2>&1; then
    BEAD_JSON="$(br --db "${BR_DB:-$HOME/.beads/beads.db}" show "$BEAD_ID" --json 2>/dev/null || true)"
  fi
fi
BEAD_DESC="$(printf '%s' "$BEAD_JSON" | python3 -c '
import json, sys
try:
    d = json.loads(sys.stdin.read() or "{}")
except Exception:
    d = {}
desc = (d.get("description") or "").strip()
acc = (d.get("acceptance_criteria") or "").strip()
if desc and acc:
    print(desc + "\n\nAcceptance:\n" + acc)
elif desc:
    print(desc)
elif acc:
    print("Acceptance:\n" + acc)
else:
    print("(no description on bead)")
' 2>/dev/null || echo '(br show --json unavailable)')"

# /goal is a built-in slash for both Claude Code and Codex. Prepending it
# activates structured goal-tracking in the spawned worker; the bead's
# description + acceptance are appended so the worker reads the goal
# artifact rather than re-deriving it from IDs.
PROMPT="/goal
Factory bead ${BEAD_ID}: drive PR #${PR} on ${TARGET_REPO} to /green + /er. Push to existing branch only; do NOT open new PR; do NOT merge.

--- Bead goal artifact (br show --json) ---
${BEAD_DESC}"

# Optional Slack pickup announcement (no-op when libnotify-slack.sh or env unset).
if [ -r "$ROOT/daemon/scripts/libnotify-slack.sh" ]; then
  # shellcheck disable=SC1091
  . "$ROOT/daemon/scripts/libnotify-slack.sh"
  slack_announce ":rocket: bead \`${BEAD_ID}\` PR #${PR} on ${TARGET_REPO} — async-spawning via AO" || true
fi

start_ready_deadline() {
  READY_DEADLINE=$(( $(date +%s) + READY_TIMEOUT ))
}

ao_ready_probe() {
  local remaining
  remaining=$(( READY_DEADLINE - $(date +%s) ))
  [ "$remaining" -gt 0 ] || return 124
  timeout "$remaining" "$AO" "$@"
}

# Pre-flight: ensure AO is reachable. Bounded at 5s wallclock so the
# async path never blocks more than that on cold-start. Two failure modes
# are caught:
#   1. ao-go daemon not running → start it with a bounded retry loop.
#   2. AO binary itself broken / not executable → fail loud immediately
#      so the tick can skip the bead instead of silently queueing a doomed
#      spawn that will never produce an `ao session ls` row.
ensure_ao_daemon() {
  start_ready_deadline
  # Catch broken AO binary first: a 127 exit on any command means the
  # CLI is misconfigured (wrong path, missing exec bit, broken install).
  # Don't queue a doomed spawn in that case.
  if ! ao_ready_probe --version >/dev/null 2>&1 && ! ao_ready_probe status >/dev/null 2>&1; then
    return 1
  fi
  if [[ "$(basename "$AO")" != "ao-go" ]]; then
    # ao-ts manages its own lifecycle; binary is the daemon.
    return 0
  fi
  if ao_ready_probe status >/dev/null 2>&1; then
    return 0
  fi
  echo "[remediate] starting Go AO daemon" >&2
  nohup "$AO" daemon >> /tmp/ao-go-daemon.log 2>&1 &
  while [ $((READY_DEADLINE - $(date +%s))) -gt 0 ]; do
    if ao_ready_probe status >/dev/null 2>&1; then
      return 0
    fi
    [ $((READY_DEADLINE - $(date +%s))) -gt 0 ] && sleep 1
  done
  return 1
}

run_spawn_foreground() {
  # Returns "exit_code<TAB>output" so the caller can branch on rc.
  local out rc
  set +e
  if ao_ready_probe spawn --help 2>&1 | grep -Eq '\-\-name'; then
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
  if echo "$out" | grep -Eq 'spawned session |Session [a-z0-9_-]+ created|✓ Session|pr_open|working|spawning|claimed https://'; then
    return 0
  fi
  if ao_ready_probe session ls -p "$AO_PROJECT" 2>/dev/null | grep -E "pulls/${PR}\b" | grep -Eq "\[(spawning|running|active|working|pr_open)\]"; then
    return 0
  fi
  return 1
}

verify_active_session() {
  # A zero exit from `ao spawn` only means the CLI accepted the request. The
  # factory's DISPATCHED state is stronger: an AO session for this exact PR
  # must be visible and active. Scope every query to the selected project.
  local sessions remaining
  while :; do
    remaining=$(( READY_DEADLINE - $(date +%s) ))
    [ "$remaining" -gt 0 ] || return 1
    sessions="$(ao_ready_probe session ls -p "$AO_PROJECT" 2>/dev/null || true)"
    if printf '%s\n' "$sessions" | grep -E "pulls/${PR}\\b" | grep -Eq '\[(spawning|running|active|working|pr_open)\]'; then
      return 0
    fi
    remaining=$(( READY_DEADLINE - $(date +%s) ))
    [ "$remaining" -gt 0 ] && sleep 1
  done
  return 1
}

# ---------- SYNC PATH (original blocking behavior) ----------
if [ "$MODE" = "sync" ]; then
  MINIMAX_SYNC="$ROOT/daemon/factory-ao-minimax-sync.sh"
  if [ -x "$MINIMAX_SYNC" ]; then
    bash "$MINIMAX_SYNC" --all || echo "[remediate] WARN: MiniMax sync failed — sessions may use Anthropic OAuth" >&2
  fi
  # Best-effort daemon readiness for sync path. Spawn capability detection
  # and required session verification still share one bounded probe deadline.
  if [[ "$(basename "$AO")" == "ao-go" ]]; then
    state="$("$AO" status --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state",""))' 2>/dev/null || true)"
    if [ "$state" != "ready" ] && [ "$state" != "running" ]; then
      echo "[remediate] starting Go AO daemon" >&2
      nohup "$AO" daemon >> /tmp/ao-go-daemon.log 2>&1 &
      sleep 2
    fi
  fi
  start_ready_deadline
  result="$(run_spawn_foreground)"
  rc="${result%%$'\t'*}"
  out="${result#*$'\t'}"
  if classify_spawn_outcome "$rc" "$out" >/dev/null \
     && { [ "${AFD_REQUIRE_SESSION:-0}" != "1" ] || verify_active_session; }; then
    [ "$rc" -eq 0 ] || echo "[remediate] spawn accepted for PR #$PR (timeout=${SPAWN_TIMEOUT}s, rc=$rc)" >&2
    echo "$out"
    exit 0
  fi
  if [ "${AFD_REQUIRE_SESSION:-0}" = "1" ]; then
    echo "[remediate] spawn for PR #$PR has no verified active AO session; refusing dispatch acknowledgement" >&2
  fi
  echo "$out" >&2
  exit 1
fi

# ---------- ASYNC PATH (default; non-blocking) ----------
# Pre-flight: bounded 5s probe. Fail loud if AO is unreachable so the tick
# can skip the bead instead of silently queueing a doomed spawn.
if ! ensure_ao_daemon; then
  echo "[remediate] AO unreachable after ${READY_TIMEOUT}s readiness deadline — refusing to async-spawn" >&2
  exit 1
fi

mkdir -p "$LOG_DIR" "$STATE_DIR"
# Mark "pending" so a stale state file from a previous crashed run doesn't
# cause the next tick to misread it. Background process overwrites with final.
echo "pending" > "$STATE_FILE"

# Detach the real spawn. Background process records outcome to STATE_FILE.
(
  set +e
  start_ready_deadline
  result="$(run_spawn_foreground)"
  rc="${result%%$'\t'*}"
  out="${result#*$'\t'}"
  printf '%s' "$out" > "$SPAWN_LOG"
  if classify_spawn_outcome "$rc" "$out" >/dev/null \
     && { [ "${AFD_REQUIRE_SESSION:-0}" != "1" ] || verify_active_session; }; then
    echo "ok" > "$STATE_FILE"
  else
    if [ "${AFD_REQUIRE_SESSION:-0}" = "1" ]; then
      echo "fail:rc=$rc:session_unverified" > "$STATE_FILE"
    else
      echo "fail:rc=$rc" > "$STATE_FILE"
    fi
  fi
) >/dev/null 2>&1 &
SPAWN_PID=$!
disown "$SPAWN_PID" 2>/dev/null || true

# Fast-fail detection: poll the state file for up to AFD_ASYNC_WAIT_SEC
# before returning. Auth/project errors and broken-daemon errors typically
# fail within 1-2s of `ao spawn`; cold-start slow spawns exceed this bound
# and we proceed optimistically (the state file still records the final
# outcome for downstream observability). This prevents the dispatch-record
# step in factory-af-tick.sh from stranding a bead in DISPATCHED when the
# spawn already failed — see Codex P1 finding on PR #193.
ASYNC_WAIT_SEC="${AFD_ASYNC_WAIT_SEC:-5}"
start_ts=$(date +%s)
final_state=""
while [ $(( $(date +%s) - start_ts )) -lt "$ASYNC_WAIT_SEC" ]; do
  cur="$(cat "$STATE_FILE" 2>/dev/null || true)"
  case "$cur" in
    ok)
      final_state="ok"
      break
      ;;
    fail:*)
      final_state="$cur"
      break
      ;;
  esac
  sleep 0.2
done

echo "[remediate] async-spawned PR #$PR bead=${BEAD_ID} nonce=${DISPATCH_NONCE:-legacy} pid=${SPAWN_PID} log=${SPAWN_LOG} state=${final_state:-pending}"
case "$final_state" in
  fail:*)
    # Fast-fail detected within wait window. Refuse so dispatch-record is
    # skipped — the bead stays QUEUED and the next tick can retry.
    echo "[remediate] fast-fail detected for PR #$PR: $final_state — refusing to acknowledge dispatch" >&2
    exit 1
    ;;
  *)
    # Either the spawn succeeded within the wait window OR it's still
    # pending (cold-start slow spawn). Caller treats rc=0 as "dispatch
    # accepted"; the state file records the eventual outcome.
    exit 0
    ;;
esac
