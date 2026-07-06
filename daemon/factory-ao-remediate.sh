#!/usr/bin/env bash
# Spawn AO remediation for a factory ATTESTED bead — isolated target-repo worktree.
# Uses Go AO mirror (~/bin/ao-go) by default; TS fallback via AO_BIN=~/bin/ao-ts.
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
MINIMAX_SYNC="$ROOT/daemon/factory-ao-minimax-sync.sh"
if [ -x "$MINIMAX_SYNC" ]; then
  bash "$MINIMAX_SYNC" --all || echo "[remediate] WARN: MiniMax sync failed — sessions may use Anthropic OAuth" >&2
fi

PROMPT="Factory bead ${BEAD_ID}: drive PR #${PR} on ${TARGET_REPO} to /green + /er. Push to existing branch only; do NOT open new PR; do NOT merge."

if [[ "$AO" == *ao-go* ]] || [[ "$(basename "$AO")" == "ao-go" ]]; then
  state="$("$AO" status --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state",""))' 2>/dev/null || true)"
  if [ "$state" != "ready" ] && [ "$state" != "running" ]; then
    echo "[remediate] starting Go AO daemon" >&2
    nohup "$AO" daemon >> /tmp/ao-go-daemon.log 2>&1 &
    sleep 2
  fi
fi

set +e
if "$AO" spawn --help 2>&1 | rg -q '\-\-name'; then
  out="$(timeout "$SPAWN_TIMEOUT" "$AO" spawn --project "$AO_PROJECT" --name "$DISPLAY_NAME" --agent claude-code --claim-pr "$PR" --prompt "$PROMPT" 2>&1)"
else
  out="$(timeout "$SPAWN_TIMEOUT" "$AO" spawn --project "$AO_PROJECT" --claim-pr "$PR" --agent claude-code "$PROMPT" 2>&1)"
fi
rc=$?
set -e

if [ "$rc" -eq 0 ]; then
  echo "$out"
  exit 0
fi
if echo "$out" | rg -q 'spawned session |Session [a-z0-9_-]+ created|✓ Session|pr_open|working|spawning|claimed https://'; then
  echo "$out"
  echo "[remediate] spawn accepted for PR #$PR (timeout=${SPAWN_TIMEOUT}s, rc=$rc)" >&2
  exit 0
fi
if "$AO" session ls 2>/dev/null | rg "pulls/${PR}\b" | rg -q "\[(spawning|running|active|working|pr_open)\]"; then
  echo "[remediate] session exists for PR #$PR (timeout=${SPAWN_TIMEOUT}s, rc=$rc)" >&2
  exit 0
fi
echo "$out" >&2
exit 1
