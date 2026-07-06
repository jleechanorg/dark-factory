#!/usr/bin/env bash
# Minimal deterministic overlay — replaces decommissioned factory-lite-harness.sh
# for intake-upsert + recover-held + unstick. All CXDB mutations go here.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BR_DB="${BR_DB:-$ROOT/.beads/beads.db}"
br() { command br --db "$BR_DB" "$@"; }
DB="${AFD_DB:-$HOME/.dark-factory/daemon-cxdb.sqlite}"
LOG="${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
SCHEMA="$ROOT/daemon/contracts/schema.sql"
die() { echo "factory-overlay: $*" >&2; exit 1; }
sql() { sqlite3 -cmd '.timeout 5000' "$DB" "$@"; }
q() { printf '%s' "$1" | sed "s/'/''/g"; }
now() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }
emit() {
  local bead_id="$1" attempt="$2" state="$3" event="$4" ctx="${5:-{\}}"
  mkdir -p "$(dirname "$LOG")"
  python3 - "$bead_id" "$attempt" "$state" "$event" "$ctx" "$LOG" <<'PYEMIT'
import json, sys
from datetime import datetime, timezone
bead_id, attempt, state, event, ctx, log = sys.argv[1:7]
ctx_obj = json.loads(ctx) if ctx.strip() else {}
row = {"ts": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"), "beadId": bead_id,
       "attempt": int(attempt), "state": state, "eventType": event,
       "counts": {}, "context": ctx_obj}
open(log, "a").write(json.dumps(row) + "\n")
PYEMIT
}
valid_bead_id() { [[ "$1" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid bead_id: $1"; }

case "${1:-}" in
init)
  mkdir -p "$(dirname "$DB")"
  sql < "$SCHEMA"
  echo "ok: schema applied to $DB"
  ;;
intake-upsert)
  [ $# -eq 3 ] || die "usage: intake-upsert <bead_id> <title>"
  valid_bead_id "$2"
  exists="$(sql "SELECT count(*) FROM bead_overlay WHERE bead_id='$(q "$2")';")"
  sql "INSERT INTO bead_overlay (bead_id,state,attempt,updated_at)
       VALUES ('$(q "$2")','QUEUED',1,'$(now)') ON CONFLICT(bead_id) DO NOTHING;"
  if [ "$exists" = "0" ]; then
    emit "$2" 1 QUEUED INTAKE_BEAD_CREATED "{\"title\":$(printf '%s' "$3" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}"
    echo "created"
  else
    echo "exists"
  fi
  ;;
recover-held)
  max_attempt=10
  recovered=0
  while IFS='|' read -r bead_id attempt pr; do
    [ -n "$bead_id" ] || continue
    [ "${attempt:-0}" -lt "$max_attempt" ] || continue
    new_attempt=$(( attempt + 1 ))
    new_state="QUEUED"
    # Re-queue held beads for route+dispatch; do not skip gate assessment via ATTESTED.
    sql "UPDATE bead_overlay SET state='$new_state', attempt=$new_attempt, autonomy_secs=0, updated_at='$(now)' WHERE bead_id='$(q "$bead_id")';"
    ctx="$(python3 -c 'import json,sys; v=sys.argv[1]; print(json.dumps({"prior_state":"HUMAN_HELD","pr_number":(int(v) if v not in ("","NULL") else None)}))' "$pr")"
    emit "$bead_id" "$new_attempt" "$new_state" RECOVERED_FROM_HELD "$ctx"
    recovered=$((recovered + 1))
  done < <(sql -separator '|' "SELECT bead_id, attempt, coalesce(cast(pr_number as text),'') FROM bead_overlay WHERE state='HUMAN_HELD';")
  echo "recovered=$recovered"
  ;;
unstick-dispatching)
  n="$(sql "UPDATE bead_overlay SET state='QUEUED', updated_at='$(now)' WHERE state='DISPATCHING'; SELECT changes();")"
  echo "unstuck=$n"
  ;;
redrive-pr)
  [ $# -eq 4 ] || die "usage: redrive-pr <bead_id> <pr_number> <branch>"
  valid_bead_id "$2"
  bid="$(q "$2")"
  pr="$3"
  branch="$(q "$4")"
  attempt="$(sqlite3 -batch -noheader -cmd '.timeout 5000' "$DB" "SELECT coalesce(attempt,0)+1 FROM bead_overlay WHERE bead_id='$bid';" 2>/dev/null | tail -1)"
  [[ "$attempt" =~ ^[0-9]+$ ]] || attempt=1
  sql "INSERT INTO bead_overlay (bead_id,state,attempt,pr_number,branch,updated_at)
       VALUES ('$bid','QUEUED',$attempt,$pr,'$branch','$(now)')
       ON CONFLICT(bead_id) DO UPDATE SET state='QUEUED', attempt=$attempt, pr_number=$pr, branch='$branch', session_id=NULL, autonomy_secs=0, updated_at='$(now)';"
  ctx="$(python3 -c 'import json,sys; print(json.dumps({"pr_number":int(sys.argv[1]),"branch":sys.argv[2]}))' "$pr" "$4")"
  emit "$2" "$attempt" QUEUED REDRIVE_RESET "$ctx"
  echo "redriven $2 PR #$pr"
  ;;
park-duplicate)
  [ $# -eq 3 ] || die "usage: park-duplicate <bead_id> <reason>"
  valid_bead_id "$2"
  bid="$(q "$2")"
  attempt="$(sqlite3 -batch -noheader -cmd '.timeout 5000' "$DB" "SELECT attempt FROM bead_overlay WHERE bead_id='$bid';" 2>/dev/null | tail -1)"
  [[ "$attempt" =~ ^[0-9]+$ ]] || attempt=1
  sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$bid';"
  emit "$2" "${attempt:-1}" HUMAN_HELD PARKED_DUPLICATE_BEAD "{"reason":$(printf '%s' "$3" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}"
  echo "parked $2"
  ;;
list)
  [ $# -eq 2 ] || die "usage: list <STATE>"
  sql -json "SELECT bead_id, pr_number, branch, attempt, autonomy_secs FROM bead_overlay WHERE state='$(q "$2")';"
  ;;
*)
  die "unknown: ${1:-}. Valid: init intake-upsert recover-held unstick-dispatching redrive-pr park-duplicate list"
  ;;
esac
