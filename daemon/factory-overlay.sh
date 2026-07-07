#!/usr/bin/env bash
# factory-overlay.sh — deterministic overlay harness for auto-factory CXDB.
#
# Combines the original factory-lite-harness.sh subcommands (lost in
# commit e60b5a31b / jleechan-xrdx) with the post-decommission replacements
# (recover-held, unstick-dispatching, redrive-pr, park-duplicate).
#
# Contract: ALL sqlite3 mutations to ~/.dark-factory/daemon-cxdb.sqlite flow
# through this script. LLM skills / dispatchers NEVER touch sqlite3 directly.
# This is the executable deterministic spec the Rust daemon replaces.
#
# Subcommands (19):
#   init                          — apply schema.sql
#   intake-upsert <id> <title>    — create bead_overlay row (idempotent)
#   route-record <id> <PATH> [n]  — record LLM route verdict (SMALL|STANDARD)
#   capacity                      — print dispatchable slot count this tick
#   dispatch-record <id> <branch> — QUEUED → DISPATCHED; register branch owner
#   pr-opened <id> <n> <url>      — DISPATCHED → ATTESTED
#   autonomy-tick <secs>          — bump autonomy_secs; warn/park at threshold
#   gate-assessment <id> <pr> <j> — record 7-gate verdict; emit READY hint
#   prev-gate-assessment <pr>     — print prior (second-to-last) assessment
#   ready <id> <pr>               — terminal READY transition
#   reroll-verdict <id> <pr> <v> <r> — record reroll decision
#   park <id> <reason>            — → HUMAN_HELD (PARKED_HUMAN_HELD)
#   park-duplicate <id> <reason>  — → HUMAN_HELD (PARKED_DUPLICATE_BEAD)
#   bead-closed-check <id>        — handle br show → READY or HUMAN_HELD
#   tick-summary <role>           — emit TICK telemetry line
#   recover-held                  — HUMAN_HELD → QUEUED (max_attempt guard)
#   unstick-dispatching           — DISPATCHING → QUEUED
#   redrive-pr <id> <pr> <branch> — re-QUEUE existing PR for re-attempt
#   list <STATE>                  — print bead_overlay rows for state
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BR_DB="${BR_DB:-$ROOT/.beads/beads.db}"
br() { command br --db "$BR_DB" "$@"; }
DB="${AFD_DB:-$HOME/.dark-factory/daemon-cxdb.sqlite}"
LOG="${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
BR_BIN="${AFD_BR_BIN:-${BR_BIN:-br}}"
SCHEMA="$ROOT/daemon/contracts/schema.sql"
CONFIG="${CONFIG:-$ROOT/config/daemon.toml}"
[ -f "$CONFIG" ] || CONFIG="$ROOT/daemon/contracts/daemon.toml.example"

# Structured exit codes for failure classes. Caller (factory-af-tick.sh and other
# dispatchers) cases on $rc instead of parsing stderr substrings — ZFC-correct:
# each subcommand reports the failure class via exit code, no keyword routing.
#
#   0  success
#   1  generic / usage error (kept for backwards-compat with existing callers)
#   2  invalid arguments (usage / validation)
#   3  over capacity (capacity gate refused)
#   4  branch conflict (branch already registered to a different bead)
#   5  require_state (bead not in required state)
#   6  valid_branch / valid_pr (input format invalid)
#   7  valid_bead_id (input format invalid)
#   8  not_found (bead has no overlay row)
#   9  io_error (sqlite / fs failure)
#  10  already_applied (state transition no-op)
EX_USAGE=2
EX_OVER_CAP=3
EX_BRANCH_CONFLICT=4
EX_REQUIRE_STATE=5
EX_VALID_INPUT=6
EX_BEAD_ID=7
EX_NOT_FOUND=8
EX_IO=9
EX_NOOP=10

die() { echo "factory-overlay: $*" >&2; exit 1; }
# die_code <code> <msg> — emit stderr and exit with structured code.
# Use this instead of `die` when the caller needs to case on the failure class.
die_code() { local rc="$1"; shift; echo "factory-overlay: $*" >&2; exit "$rc"; }
cfg() {
  grep -E "^${1}[[:space:]]*=" "$CONFIG" | head -1 | sed -E 's/^[^=]+=[[:space:]]*//; s/^"//; s/"[[:space:]]*(#.*)?$//; s/[[:space:]]*(#.*)?$//'
}
sql() { sqlite3 -cmd '.timeout 5000' "$DB" "$@"; }
q() { printf '%s' "$1" | sed "s/'/''/g"; }
js() { printf '%s' "$1" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))'; }
valid_bead_id() { [[ "$1" =~ ^[A-Za-z0-9._-]+$ ]] || die_code $EX_BEAD_ID "invalid bead_id: $1"; }
valid_branch()  {
  [[ "$1" =~ ^factory/[A-Za-z0-9._-]+-r[0-9]+$ ]] && return 0
  if [[ "$1" =~ ^[A-Za-z0-9._/-]+$ ]] && [[ ! "$1" =~ ^factory/ ]] && [[ ! "$1" =~ ^refs/ ]] && [[ "$1" != "HEAD" ]]; then
    return 0
  fi
  die_code $EX_VALID_INPUT "branch must match factory/<bead_id>-r<n> OR existing-PR branch name: $1"
}
valid_pr() { [[ "$1" =~ ^[0-9]+$ ]] || die "pr_number must be numeric: $1"; }
VALID_STATES="QUEUED DISPATCHING DISPATCHED ATTESTED READY RE_ROLL RECOVERY REDISPATCHED BUDGET_HELD HUMAN_HELD"
valid_state() { case " $VALID_STATES " in *" $1 "*) ;; *) die "invalid state: $1";; esac; }
now() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }
get_field() { sql "SELECT $2 FROM bead_overlay WHERE bead_id='$(q "$1")';"; }
require_state() {
  local cur; cur="$(get_field "$1" state)"; shift
  case " $* " in *" $cur "*) ;; *) die "bead in state '$cur', expected one of: $*";;
  esac
}
require_pr() { local stored; stored="$(get_field "$1" pr_number)"; [ "$stored" = "$2" ] || die "pr_number $2 != overlay row's $stored for $1"; }

emit() { # emit <bead_id> <attempt> <state> <event> <ctx_json>
  local bead_id="$1" attempt="$2" state="$3" event="$4" ctx="$5"
  mkdir -p "$(dirname "$LOG")"
  printf '%s' "$ctx" | python3 -c 'import json,sys; json.load(sys.stdin)' >/dev/null 2>&1 || die "context not JSON: $ctx"
  python3 - "$bead_id" "$attempt" "$state" "$event" "$ctx" "$LOG" <<'PYEMIT'
import json, sys
from datetime import datetime, timezone
bead_id, attempt, state, event, ctx, log = sys.argv[1:7]
ctx_obj = json.loads(ctx) if ctx.strip() else {}
row = {"ts": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
       "beadId": bead_id, "attempt": int(attempt), "state": state,
       "eventType": event, "counts": {}, "context": ctx_obj}
open(log, "a").write(json.dumps(row) + "\n")
PYEMIT
}

case "${1:-}" in
init)
  mkdir -p "$(dirname "$DB")"
  sql < "$SCHEMA"
  echo "ok: schema applied to $DB"
  ;;

intake-upsert)
  [ $# -eq 3 ] || die "usage: intake-upsert <bead_id> <title>"
  valid_bead_id "$2"
  local_exists="$(sql "SELECT count(*) FROM bead_overlay WHERE bead_id='$(q "$2")';")"
  sql "INSERT INTO bead_overlay (bead_id,state,attempt,updated_at)
       VALUES ('$(q "$2")','QUEUED',1,'$(now)') ON CONFLICT(bead_id) DO NOTHING;"
  if [ "$local_exists" = "0" ]; then
    emit "$2" 1 QUEUED INTAKE_BEAD_CREATED "{\"title\":$(js "$3")}"
    echo "created"
  else
    echo "exists"
  fi
  ;;

route-record)
  [ $# -ge 3 ] || die "usage: route-record <bead_id> <verdict> [note]"
  valid_bead_id "$2"
  case "$3" in SMALL_PATH|STANDARD_PATH) ;; *) die "invalid routing verdict: $3";; esac
  require_state "$2" QUEUED
  cur_attempt="$(get_field "$2" attempt)"
  note="${4:-}"
  emit "$2" "$cur_attempt" QUEUED TASK_ROUTED "{\"routingVerdict\":\"$3\",\"note\":$(js "$note")}"
  echo "ok"
  ;;

capacity)
  active="$(sql "SELECT count(*) FROM bead_overlay WHERE state IN ('DISPATCHED','ATTESTED');")"
  mw="$(cfg max_workers)"; mb="$(cfg max_batch)"
  mw="${mw:-30}"; mb="${mb:-15}"
  free=$(( mw - active )); [ "$free" -lt 0 ] && free=0
  [ "$free" -gt "$mb" ] && free="$mb"
  echo "$free"
  ;;

dispatch-record)
  [ $# -eq 3 ] || die_code $EX_USAGE "usage: dispatch-record <bead_id> <branch>"
  valid_bead_id "$2" || die_code $EX_BEAD_ID "invalid bead_id: $2"
  valid_branch "$3"   || die_code $EX_VALID_INPUT "invalid branch: $3"
  # require_state uses die() (not die_code) to preserve the existing require_state
  # contract; callers should case on $rc == $EX_REQUIRE_STATE only when they
  # specifically need to distinguish require_state from other failures.
  if ! cur="$(get_field "$2" state)" 2>/dev/null; then
    die_code $EX_IO "state lookup failed for $2"
  fi
  case " QUEUED " in *" $cur "*) ;; *) die_code $EX_REQUIRE_STATE "bead in state '$cur', expected one of: QUEUED";; esac
  cur_cap="$("$0" capacity)"
  [ "$cur_cap" -gt 0 ] || die_code $EX_OVER_CAP "over capacity — dispatch refused (capacity=$cur_cap)"
  owner="$(sql "SELECT bead_id FROM branch_registry WHERE branch='$(q "$3")';")"
  if [ -n "$owner" ] && [ "$owner" != "$2" ]; then
    die_code $EX_BRANCH_CONFLICT "branch $3 already registered to $owner"
  fi
  sql "INSERT INTO branch_registry (branch,bead_id,created_at)
       VALUES ('$(q "$3")','$(q "$2")','$(now)') ON CONFLICT(branch) DO NOTHING;
       UPDATE bead_overlay SET state='DISPATCHED', branch='$(q "$3")', updated_at='$(now)'
       WHERE bead_id='$(q "$2")';"
  cur_attempt="$(get_field "$2" attempt)"
  emit "$2" "$cur_attempt" DISPATCHED TASK_DISPATCHED "{\"activeModel\":\"minimax\",\"branch\":$(js "$3")}"
  echo "ok"
  ;;

pr-opened)
  [ $# -eq 4 ] || die "usage: pr-opened <bead_id> <pr_number> <url>"
  valid_bead_id "$2"
  valid_pr "$3"
  require_state "$2" DISPATCHED
  sql "UPDATE bead_overlay SET state='ATTESTED', pr_number=$3, updated_at='$(now)' WHERE bead_id='$(q "$2")';"
  cur_attempt="$(get_field "$2" attempt)"
  emit "$2" "$cur_attempt" ATTESTED PR_OPENED "{\"pr_number\":$3,\"url\":$(js "$4")}"
  echo "ok"
  ;;

autonomy-tick)
  [ $# -eq 2 ] || die "usage: autonomy-tick <elapsed_secs>"
  [[ "$2" =~ ^[0-9]+$ ]] || die "elapsed_secs must be numeric"
  box="$(cfg autonomy_timebox_secs)"; box="${box:-10800}"; warn=$(( box * 8 / 10 ))
  sql "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + $2, updated_at='$(now)'
       WHERE state IN ('DISPATCHED','ATTESTED');"
  warned=0; parked=0
  while IFS='|' read -r bead_id; do
    [ -n "$bead_id" ] || continue
    if [ "$warned" -lt 1 ]; then
      cur_state="$(get_field "$bead_id" state)"
      cur_attempt="$(get_field "$bead_id" attempt)"
      [[ "$cur_attempt" =~ ^[0-9]+$ ]] || cur_attempt=1
      emit "$bead_id" "$cur_attempt" "$cur_state" AUTONOMY_WARN "{\"threshold_secs\":$warn,\"box_secs\":$box}"
      warned=1
    fi
  done < <(sql -separator '|' "SELECT bead_id FROM bead_overlay WHERE state IN ('DISPATCHED','ATTESTED') AND autonomy_secs >= $warn AND autonomy_secs <= $box AND autonomy_secs - $2 < $warn;")
  while IFS='|' read -r bead_id; do
    [ -n "$bead_id" ] || continue
    cur_state="$(get_field "$bead_id" state)"
    cur_attempt="$(get_field "$bead_id" attempt)"
    [[ "$cur_attempt" =~ ^[0-9]+$ ]] || cur_attempt=1
    sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$bead_id")';"
    emit "$bead_id" "$cur_attempt" HUMAN_HELD PARKED_AUTONOMY "{\"autonomy_secs\":$(get_field "$bead_id" autonomy_secs),\"box_secs\":$box}"
    parked=$((parked + 1))
  done < <(sql -separator '|' "SELECT bead_id FROM bead_overlay WHERE state IN ('DISPATCHED','ATTESTED') AND autonomy_secs > $box;")
  echo "warned=$warned parked=$parked"
  ;;

gate-assessment)
  [ $# -eq 4 ] || die "usage: gate-assessment <bead_id> <pr_number> <gates_json>"
  valid_bead_id "$2"
  valid_pr "$3"
  require_state "$2" ATTESTED
  require_pr "$2" "$3"
  python3 - "$4" <<'PYGA' || die "invalid gates json"
import json, sys
g = json.loads(sys.argv[1])
keys = {"ci_green","no_conflicts","coderabbit","bugbot","comments_resolved","evidence_review","skeptic"}
assert set(g) == keys, f"gate keys must be exactly {sorted(keys)}, got {sorted(g)}"
assert all(v in ("green","red","unknown") for v in g.values()), "gate values must be green|red|unknown"
PYGA
  all_green="$(python3 -c 'import json,sys; g=json.loads(sys.argv[1]); print("true" if all(v=="green" for v in g.values()) else "false")' "$4")"
  prior_last="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$3[,}]" | tail -1 || true)"
  cooldown="false"
  if [ -n "$prior_last" ] && printf '%s' "$prior_last" | grep -q '"all_green": false'; then cooldown="true"; fi
  cur_attempt="$(get_field "$2" attempt)"
  emit "$2" "$cur_attempt" ATTESTED GATE_ASSESSMENT "{\"pr_number\":$3,\"gates\":$4,\"all_green\":$all_green}"
  echo "$all_green"
  echo "cooldown_ready=$cooldown"
  ;;

prev-gate-assessment)
  [ $# -eq 2 ] || die "usage: prev-gate-assessment <pr_number>"
  valid_pr "$2"
  m="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$2[,}]" || true)"
  if [ "$(printf '%s\n' "$m" | grep -c .)" -ge 2 ]; then printf '%s\n' "$m" | tail -2 | head -1; fi
  ;;

ready)
  [ $# -eq 3 ] || die "usage: ready <bead_id> <pr_number>"
  valid_bead_id "$2"
  valid_pr "$3"
  require_state "$2" ATTESTED
  require_pr "$2" "$3"
  cur_attempt="$(get_field "$2" attempt)"
  sql "UPDATE bead_overlay SET state='READY', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
  emit "$2" "$cur_attempt" READY READY_FOR_MERGE "{\"pr_number\":$3}"
  echo "ok"
  ;;

reroll-verdict)
  [ $# -eq 5 ] || die "usage: reroll-verdict <bead_id> <pr_number> <verdict> <rationale>"
  valid_bead_id "$2"
  valid_pr "$3"
  case "$4" in in_place_fixable|reroll_worthy) ;; *) die "invalid verdict: $4";; esac
  require_state "$2" ATTESTED
  require_pr "$2" "$3"
  cur_attempt="$(get_field "$2" attempt)"
  rat_json="$(js "$5")"
  emit "$2" "$cur_attempt" ATTESTED REROLL_VERDICT_RECORDED "{\"pr_number\":$3,\"verdict\":\"$4\",\"rationale\":$rat_json}"
  if [ "$4" = "reroll_worthy" ]; then
    sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
    emit "$2" "$cur_attempt" HUMAN_HELD PARKED_HUMAN_HELD '{"reason":"reroll_worthy_stage1_disabled"}'
  fi
  echo "ok"
  ;;

park)
  [ $# -eq 3 ] || die "usage: park <bead_id> <reason>"
  valid_bead_id "$2"
  cur_attempt="$(get_field "$2" attempt)"
  [[ "$cur_attempt" =~ ^[0-9]+$ ]] || cur_attempt=1
  sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
  emit "$2" "$cur_attempt" HUMAN_HELD PARKED_HUMAN_HELD "{\"reason\":$(js "$3")}"
  echo "ok"
  ;;

park-duplicate)
  [ $# -eq 3 ] || die "usage: park-duplicate <bead_id> <reason>"
  valid_bead_id "$2"
  cur_attempt="$(get_field "$2" attempt)"
  [[ "$cur_attempt" =~ ^[0-9]+$ ]] || cur_attempt=1
  sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
  emit "$2" "$cur_attempt" HUMAN_HELD PARKED_DUPLICATE_BEAD "{\"reason\":$(js "$3")}"
  echo "parked $2"
  ;;

bead-closed-check)
  [ $# -eq 2 ] || die "usage: bead-closed-check <bead_id>"
  bead_id="$2"
  valid_bead_id "$bead_id"
  [ -n "$(get_field "$bead_id" bead_id)" ] || die "unknown bead_id (no overlay row): $bead_id"
  br_json="$("$BR_BIN" show "$bead_id" --json 2>/dev/null)" || die "br show failed for $bead_id"
  status="$(printf '%s' "$br_json" | python3 -c 'import json,sys
d = json.load(sys.stdin)
if not d: print("missing"); sys.exit(0)
print((d[0] if isinstance(d, list) else d).get("status","unknown"))' 2>/dev/null || echo unknown)"
  cur_state="$(get_field "$bead_id" state)"
  cur_attempt="$(get_field "$bead_id" attempt)"
  [[ "$cur_attempt" =~ ^[0-9]+$ ]] || cur_attempt=1
  if [ "$status" != "closed" ]; then echo "not_closed"; exit 0; fi
  if [ "$cur_state" = "READY" ] || [ "$cur_state" = "HUMAN_HELD" ]; then
    echo "already_terminal"; exit 0
  fi
  if [ "$cur_state" = "DISPATCHED" ] || [ "$cur_state" = "ATTESTED" ]; then
    branch="$(get_field "$bead_id" branch)"
    pr_number="$(get_field "$bead_id" pr_number)"
    merged_ok=""
    if [ -n "$pr_number" ] && [ "$pr_number" != "None" ]; then
      last_ga="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$pr_number[,}]" | tail -1 || true)"
      if [ -n "$last_ga" ] && printf '%s' "$last_ga" | python3 -c 'import json,sys
try: g=json.loads(sys.stdin.read())["context"]["gates"]
except Exception: sys.exit(1)
sys.exit(0 if not any(v=="red" for v in g.values()) else 1)'; then
        merged_ok="yes"
      fi
    fi
    if [ -n "$merged_ok" ]; then
      sql "UPDATE bead_overlay SET state='READY', updated_at='$(now)' WHERE bead_id='$(q "$bead_id")';"
      emit "$bead_id" "$cur_attempt" READY READY_FOR_MERGE "{\"pr_number\":$pr_number,\"note\":\"closed_after_merge\"}"
      echo "ready"
    else
      sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$bead_id")';"
      ctx="$(python3 -c 'import json,sys; print(json.dumps({"reason":"bead_closed_underneath","prior_state":sys.argv[1],"branch":(sys.argv[2] or None),"pr_number":(int(sys.argv[3]) if sys.argv[3] not in ("", "None") else None)}))' "$cur_state" "$branch" "$pr_number")"
      emit "$bead_id" "$cur_attempt" HUMAN_HELD PARKED_HUMAN_HELD "$ctx"
      echo "parked"
    fi
  else
    echo "already_terminal"
  fi
  ;;

tick-summary)
  [ $# -eq 2 ] || die "usage: tick-summary <coder|verifier>"
  counts="$(sql -json "SELECT state, count(*) AS n FROM bead_overlay GROUP BY state;" 2>/dev/null | python3 -c 'import json,sys
try:
  d=json.load(sys.stdin)
  print(json.dumps({r["state"].lower(): r["n"] for r in d}))
except Exception: print("{}")' 2>/dev/null || echo '{}')"
  emit "tick" 0 "N/A" TICK "{\"by_state\":$counts}" "{\"role\":\"$2\"}"
  echo "ok"
  ;;

recover-held)
  max_attempt=10
  recovered=0
  while IFS='|' read -r bead_id attempt pr; do
    [ -n "$bead_id" ] || continue
    [ "${attempt:-0}" -lt "$max_attempt" ] || continue
    new_attempt=$(( attempt + 1 ))
    new_state="QUEUED"
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

list)
  [ $# -eq 2 ] || die "usage: list <STATE>"
  valid_state "$2"
  sql -json "SELECT bead_id, pr_number, branch, attempt, autonomy_secs FROM bead_overlay WHERE state='$(q "$2")';"
  ;;

*)
  die "unknown: ${1:-}. Valid: init intake-upsert route-record capacity dispatch-record pr-opened autonomy-tick gate-assessment prev-gate-assessment ready reroll-verdict park park-duplicate bead-closed-check tick-summary recover-held unstick-dispatching redrive-pr list"
  ;;
esac