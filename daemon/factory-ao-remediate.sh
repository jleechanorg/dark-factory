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
#   AFD_ASYNC_WAIT_SEC        how long the async wrapper polls the state
#                             file for fast-fail detection before returning
#                             optimistically (default 5). Most auth/project
#                             errors fail within 1-2s; cold-start slow spawns
#                             exceed this bound and proceed optimistically.
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
DB="${AFD_DB:-$HOME/.dark-factory/daemon-cxdb.sqlite}"

# Bead dark-factory-w2fr: compute the target-identity tokens the worker
# session will need to validate its worktree / branch / repo before
# writing any tracked file. These four env vars are injected into the AO
# spawn so they reach the worker session; `daemon/scripts/af-target-identity-guard.sh`
# reads them at every pre-write / pre-push check.
#
# AF_TARGET_CHECKOUT: the absolute path of the daemon-managed target
#   checkout for $TARGET_REPO, resolved from the same `[repos]`
#   routing table the Rust adapter consults (see
#   `Config::resolve_repo` / `daemon/src/config.rs`). Failing closed
#   means we deliberately do NOT default to $ROOT or any guess — a
#   remediation worker without an authoritative checkout path is
#   exactly the silent-drift condition we are trying to surface.
CONFIG="${CONFIG:-$ROOT/config/daemon.toml}"
[ -f "$CONFIG" ] || CONFIG="$ROOT/daemon/contracts/daemon.toml.example"

# Resolve AF_TARGET_CHECKOUT from the [repos] table; fall back to the
# global default_repo's `local_checkout` only when present (legacy
# single-repo deployments). When neither path is known, AF_TARGET_CHECKOUT
# is left empty — `af-target-identity-guard.sh` treats that as a
# fail-closed missing-env condition (no silent pass).
AF_TARGET_CHECKOUT="$(TARGET_REPO="$TARGET_REPO" CONFIG="$CONFIG" python3 -c '
import os, sys
try:
    import tomllib
except ImportError:
    try:
        import tomli as tomllib
    except ImportError:
        sys.exit(0)
cfg_path = os.environ.get("CONFIG", "")
target = os.environ.get("TARGET_REPO", "")
try:
    with open(cfg_path, "rb") as fp:
        cfg = tomllib.load(fp)
except Exception:
    sys.exit(0)
repos = cfg.get("repos") or {}
entry = repos.get(target) or {}
local = entry.get("local_checkout")
if local:
    print(local)
    sys.exit(0)
sys.exit(0)
')"

# AF_TARGET_BRANCH: the PR's head branch. Look it up via `gh pr view`
# first (the source of truth the worker will land on); fall back to the
# bead's stored branch from the SQLite overlay (the path the daemon
# already validated as part of `dispatch-record`). `gh pr view` failures
# are non-fatal — the worker will still get the repo + checkout token,
# and its own `af-target-identity-guard.sh` will surface the missing
# branch via a drift sentinel. Final fallback: the factory-fabricated
# default `refs/heads/factory/<BEAD_ID>-r1` (the branch the dispatch
# path uses for an ordinary create-new-work bead); if THAT is wrong, the
# guard catches it on the first pre-write / pre-push invocation.
AF_TARGET_BRANCH=""
if command -v gh >/dev/null 2>&1; then
  AF_TARGET_BRANCH="$(gh pr view "$PR" --repo "$TARGET_REPO" --json headRefName -q '.headRefName // empty' 2>/dev/null || true)"
  if [ -n "$AF_TARGET_BRANCH" ]; then
    AF_TARGET_BRANCH="refs/heads/${AF_TARGET_BRANCH}"
  fi
fi
if [ -z "$AF_TARGET_BRANCH" ] && [ -r "$DB" ]; then
  AF_TARGET_BRANCH="$(sqlite3 "$DB" "SELECT branch FROM bead_overlay WHERE bead_id='$(printf "%s" "$BEAD_ID" | sed "s/'/''/g")' AND branch IS NOT NULL;" 2>/dev/null || true)"
fi
# Normalize: if the resolved branch is bare (no `refs/heads/` prefix),
# add it. The guard's `normalize_branch` accepts both forms, but
# canonicalizing here makes the sentinel + drift message easier to read.
case "$AF_TARGET_BRANCH" in
  refs/heads/*) ;;
  "") AF_TARGET_BRANCH="refs/heads/factory/${BEAD_ID}-r1" ;;
  *) AF_TARGET_BRANCH="refs/heads/${AF_TARGET_BRANCH}" ;;
esac

# AF_TARGET_REPO: passed-through canonical form the guard normalizes
# (lowercase, strip `.git`). Set from $3 (the explicit target_repo arg)
# so the worker has the same owner/name the dispatch path chose.
AF_TARGET_REPO="$TARGET_REPO"

export AF_TARGET_CHECKOUT AF_TARGET_BRANCH AF_TARGET_REPO AF_BEAD_ID AF_PR_NUMBER
export AF_BEAD_ID="$BEAD_ID" AF_PR_NUMBER="$PR"

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
#
# Bead dark-factory-w2fr: the prompt also embeds the target-identity
# preamble so the worker is told (in plain language) to validate its
# worktree / branch / repo BEFORE writing any tracked file or running
# `git push`, and to invoke
# `daemon/scripts/af-target-identity-guard.sh` (which reads the
# AF_TARGET_* tokens injected below) on each such event. The
# dark-factory-o74s / wa-3551 incident reproduced by this bead
# (a remediation worker for PR #9462 that ended up editing
# provenance-narrow/mvp_site/schemas/prompt_tool_contracts.json — a
# path in a different repo from the assignment) is exactly what this
# guard is designed to refuse.
PROMPT="/goal
Factory bead ${BEAD_ID}: drive PR #${PR} on ${TARGET_REPO} to /green + /er. Push to existing branch only; do NOT open new PR; do NOT merge.

# TARGET IDENTITY (bead dark-factory-w2fr)
# Before writing any tracked file or running \`git push\`, you MUST verify
# your worker's identity matches the assignment below. Run:
#
#     bash daemon/scripts/af-target-identity-guard.sh
#
# (or invoke the pre-push wrapper
# \`daemon/scripts/af-push-identity-guard.sh\` instead of \`git push\`
# directly — it runs the same check then exec's the underlying push).
# The guard reads AF_TARGET_CHECKOUT / AF_TARGET_BRANCH / AF_TARGET_REPO
# from your environment (set by the factory at spawn time) and refuses
# the action on any mismatch. Failure is fail-closed: you halt on a
# drift sentinel and the bead is parked HUMAN_HELD — the operator must
# reconcile the worktree / branch / repo identity before any further
# dispatch.
#
#   Assigned worktree: \${AF_TARGET_CHECKOUT}
#   Assigned branch:   \${AF_TARGET_BRANCH}
#   Assigned repo:     \${AF_TARGET_REPO}
#   Assigned bead:     ${BEAD_ID}
#   Assigned PR:       #${PR}

--- Bead goal artifact (br show --json) ---
${BEAD_DESC}"

# Optional Slack pickup announcement (no-op when libnotify-slack.sh or env unset).
if [ -r "$ROOT/daemon/scripts/libnotify-slack.sh" ]; then
  # shellcheck disable=SC1091
  . "$ROOT/daemon/scripts/libnotify-slack.sh"
  slack_announce ":rocket: bead \`${BEAD_ID}\` PR #${PR} on ${TARGET_REPO} — async-spawning via AO" || true
fi

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
  if "$AO" spawn --help 2>&1 | grep -Eq '\-\-name'; then
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
  if "$AO" session ls 2>/dev/null | grep -E "pulls/${PR}\b" | grep -Eq "\[(spawning|running|active|working|pr_open)\]"; then
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

echo "[remediate] async-spawned PR #$PR bead=${BEAD_ID} pid=${SPAWN_PID} log=${SPAWN_LOG} state=${final_state:-pending}"
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