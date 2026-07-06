#!/usr/bin/env bash
# Deterministic /af one tick: intake + recover + AO dispatch for ATTESTED drive beads.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BR_DB="${BR_DB:-$ROOT/.beads/beads.db}"
br() { command br --db "$BR_DB" "$@"; }
O="$ROOT/daemon/factory-overlay.sh"
I="$ROOT/daemon/factory-intake-from-gh.sh"
R="$ROOT/daemon/factory-ao-remediate.sh"
CONFIG="$ROOT/config/daemon.toml"
MAX_DISPATCH="${MAX_DISPATCH:-2}"

cd "$ROOT"
"$O" init
"$O" unstick-dispatching
"$O" recover-held
bash "$I"

AO="$(bash "$ROOT/daemon/factory-ao-bin.sh" 2>/dev/null || true)"
AO_CAP="${AO_MAX_CONCURRENT_SESSIONS:-30}"
if [ -n "$AO" ]; then
  if "$AO" session ls --json >/dev/null 2>&1; then
    ao_active="$("$AO" session ls --json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(1 for s in d.get("data",[]) if not s.get("isTerminated")))' 2>/dev/null || echo 0)"
  else
    ao_active="$("$AO" session ls 2>/dev/null | rg -c '\[(spawning|running|active|working|pr_open)\]' || echo 0)"
  fi
  if [ "${ao_active:-0}" -ge "$AO_CAP" ]; then
    echo "[af] AO cap: ${ao_active} active >= ${AO_CAP} — skipping dispatch (intake done)" >&2
    MAX_DISPATCH=0
  fi
fi

dispatched=0
while IFS=$'\t' read -r bead_id pr branch; do
  [ -n "$bead_id" ] || continue
  [ "$dispatched" -ge "$MAX_DISPATCH" ] && break
  echo "[af] remediate $bead_id PR #$pr"
  if bash "$R" "$bead_id" "$pr" 2>&1; then
    dispatched=$((dispatched+1))
  else
    echo "[af] skip $bead_id (ao spawn failed)" >&2
  fi
done < <(sqlite3 "$HOME/.dark-factory/daemon-cxdb.sqlite" -separator $'\t' \
  "SELECT bead_id, pr_number, coalesce(branch,'') FROM bead_overlay WHERE state='ATTESTED' AND pr_number IS NOT NULL ORDER BY CASE WHEN bead_id LIKE 'jleechan-%' THEN 0 ELSE 1 END, updated_at LIMIT 10;")

echo "af_dispatched=$dispatched"
callpath run dark-factory ${1+"$@"} 2>/dev/null || true
