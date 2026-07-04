#!/usr/bin/env bash
# factory-lite deterministic harness — ALL binding mutations go through here.
# The LLM skills call these subcommands and supply only typed judgment verdicts;
# they never touch sqlite3/telemetry directly. NEVER-rule enforcement is structural:
# this script contains no merge, push, force-push, or branch-delete code path.
# This harness is the executable deterministic spec the Rust daemon replaces.
set -euo pipefail

DB="${AFD_DB:-$HOME/.dark-factory/daemon-cxdb.sqlite}"
LOG="${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
BR_BIN="${AFD_BR_BIN:-br}"
REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SCHEMA="$REPO_DIR/daemon/contracts/schema.sql"
CONFIG="$REPO_DIR/config/daemon.toml"
[ -f "$CONFIG" ] || CONFIG="$REPO_DIR/daemon/contracts/daemon.toml.example"

die() { echo "harness: $*" >&2; exit 1; }

cfg() { # cfg <key> — naive toml scalar read (deterministic; lite-only)
  grep -E "^${1}[[:space:]]*=" "$CONFIG" | head -1 | sed -E 's/^[^=]+=[[:space:]]*//; s/^"//; s/"[[:space:]]*(#.*)?$//; s/[[:space:]]*(#.*)?$//'
}

sql() { sqlite3 -cmd '.timeout 5000' "$DB" "$@"; }  # per-connection busy_timeout (PRAGMA in schema only binds the init conn)
q() { printf '%s' "$1" | sed "s/'/''/g"; }  # sqlite string escape
js() { printf '%s' "$1" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))'; }  # safe JSON string literal
valid_bead_id() { [[ "$1" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid bead_id (must be [A-Za-z0-9._-]): $1"; }
valid_branch()  { [[ "$1" =~ ^factory/[A-Za-z0-9._-]+-r[0-9]+$ ]] || die "branch must match factory/<bead_id>-r<n>: $1"; }
valid_pr()      { [[ "$1" =~ ^[0-9]+$ ]] || die "pr_number must be numeric: $1"; }
# assert the caller-supplied pr matches the overlay row (drift guard)
require_pr() { local stored; stored="$(get_field "$1" pr_number)"; [ "$stored" = "$2" ] || die "pr_number $2 != overlay row's $stored for $1"; }

VALID_STATES="QUEUED DISPATCHED ATTESTED READY RE_ROLL RECOVERY REDISPATCHED BUDGET_HELD HUMAN_HELD"
valid_state() { case " $VALID_STATES " in *" $1 "*) ;; *) die "invalid state: $1";; esac; }

now() { date -u +%FT%TZ; }

emit() { # emit <bead_id> <attempt> <lifecycle_state> <event_type> <metrics_json> <context_json>
  mkdir -p "$(dirname "$LOG")"
  local metrics="$5" context="$6"
  echo "$metrics" | python3 -c 'import json,sys; json.load(sys.stdin)' >/dev/null || die "metrics not JSON"
  echo "$context" | python3 -c 'import json,sys; json.load(sys.stdin)' >/dev/null || die "context not JSON"
  python3 - "$1" "$2" "$3" "$4" "$metrics" "$context" >> "$LOG" <<'PY'
import json, sys, datetime
bead, attempt, state, etype, metrics, context = sys.argv[1:7]
print(json.dumps({
  "timestamp": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
  "beadId": bead, "attemptId": int(attempt), "lifecycleState": state,
  "eventType": etype, "metrics": json.loads(metrics), "context": json.loads(context)}))
PY
}

get_field() { sql "SELECT $2 FROM bead_overlay WHERE bead_id='$(q "$1")';"; }
require_state() { # require_state <bead_id> <expected...>
  local cur; cur="$(get_field "$1" state)"; shift
  case " $* " in *" $cur "*) ;; *) die "bead in state '$cur', expected one of: $*";; esac
}

case "${1:-}" in

init)
  mkdir -p "$(dirname "$DB")"
  sql < "$SCHEMA"
  echo "ok: schema applied to $DB"
  ;;

intake-upsert) # intake-upsert <bead_id> <title>
  [ $# -eq 3 ] || die "usage: intake-upsert <bead_id> <title>"
  valid_bead_id "$2"
  local_exists="$(sql "SELECT count(*) FROM bead_overlay WHERE bead_id='$(q "$2")';")"
  sql "INSERT INTO bead_overlay (bead_id,state,attempt,updated_at)
       VALUES ('$(q "$2")','QUEUED',1,'$(now)') ON CONFLICT(bead_id) DO NOTHING;"
  if [ "$local_exists" = "0" ]; then
    emit "$2" 1 QUEUED INTAKE_BEAD_CREATED '{}' "{\"title\": $(printf '%s' "$3" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}"
    echo "created"
  else
    echo "exists"
  fi
  ;;

route-record) # route-record <bead_id> <SMALL_PATH|STANDARD_PATH> [note]
  [ $# -ge 3 ] || die "usage: route-record <bead_id> <verdict> [note]"
  case "$3" in SMALL_PATH|STANDARD_PATH) ;; *) die "invalid routing verdict: $3";; esac
  require_state "$2" QUEUED
  emit "$2" "$(get_field "$2" attempt)" QUEUED TASK_ROUTED '{}' "{\"routingVerdict\":\"$3\",\"note\":$(js "${4:-}")}"
  echo "ok"
  ;;

capacity) # capacity — prints dispatchable slot count this tick
  active="$(sql "SELECT count(*) FROM bead_overlay WHERE state IN ('DISPATCHED','ATTESTED');")"
  mw="$(cfg max_workers)"; mb="$(cfg max_batch)"
  free=$(( mw - active )); [ "$free" -lt 0 ] && free=0
  [ "$free" -gt "$mb" ] && free="$mb"
  echo "$free"
  ;;

dispatch-record) # dispatch-record <bead_id> <branch>
  [ $# -eq 3 ] || die "usage: dispatch-record <bead_id> <branch>"
  valid_bead_id "$2"; valid_branch "$3"
  require_state "$2" QUEUED
  [ "$("$0" capacity)" -gt 0 ] || die "over capacity — dispatch refused"
  owner="$(sql "SELECT bead_id FROM branch_registry WHERE branch='$(q "$3")';")"
  [ -z "$owner" ] || [ "$owner" = "$2" ] || die "branch $3 already registered to $owner"
  sql "INSERT INTO branch_registry (branch,bead_id,created_at)
       VALUES ('$(q "$3")','$(q "$2")','$(now)') ON CONFLICT(branch) DO NOTHING;
       UPDATE bead_overlay SET state='DISPATCHED', branch='$(q "$3")', updated_at='$(now)'
       WHERE bead_id='$(q "$2")';"
  emit "$2" "$(get_field "$2" attempt)" DISPATCHED TASK_DISPATCHED '{}' "{\"activeModel\":\"minimax\",\"branch\":$(js "$3")}"
  echo "ok"
  ;;

pr-opened) # pr-opened <bead_id> <pr_number> <url>
  [ $# -eq 4 ] || die "usage: pr-opened <bead_id> <pr_number> <url>"
  [[ "$3" =~ ^[0-9]+$ ]] || die "pr_number must be numeric"
  require_state "$2" DISPATCHED
  sql "UPDATE bead_overlay SET state='ATTESTED', pr_number=$3, updated_at='$(now)' WHERE bead_id='$(q "$2")';"
  emit "$2" "$(get_field "$2" attempt)" ATTESTED PR_OPENED '{}' "{\"pr_number\":$3,\"url\":$(js "$4")}"
  echo "ok"
  ;;

autonomy-tick) # autonomy-tick <elapsed_secs> — increments actives, warns at 80%, parks over box
  [ $# -eq 2 ] || die "usage: autonomy-tick <elapsed_secs>"
  [[ "$2" =~ ^[0-9]+$ ]] || die "elapsed_secs must be numeric"
  box="$(cfg autonomy_timebox_secs)"; warn=$(( box * 8 / 10 ))
  sql "UPDATE bead_overlay SET autonomy_secs = autonomy_secs + $2, updated_at='$(now)'
       WHERE state IN ('DISPATCHED','ATTESTED');"
  for b in $(sql "SELECT bead_id FROM bead_overlay WHERE state IN ('DISPATCHED','ATTESTED') AND autonomy_secs >= $warn AND autonomy_secs <= $box AND autonomy_secs - $2 < $warn;"); do
    emit "$b" "$(get_field "$b" attempt)" "$(get_field "$b" state)" BUDGET_WARNING "{\"elapsedAutonomySeconds\":$(get_field "$b" autonomy_secs)}" '{"reason":"autonomy_80pct"}'
  done
  for b in $(sql "SELECT bead_id FROM bead_overlay WHERE state IN ('DISPATCHED','ATTESTED') AND autonomy_secs > $box;"); do
    sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$b")';"
    emit "$b" "$(get_field "$b" attempt)" HUMAN_HELD PARKED_HUMAN_HELD "{\"elapsedAutonomySeconds\":$(get_field "$b" autonomy_secs)}" '{"reason":"autonomy_timebox_exceeded"}'
  done
  echo "ok"
  ;;

gate-assessment) # gate-assessment <bead_id> <pr_number> <gates_json>  (gates: 7 keys, green|red|unknown)
  [ $# -eq 4 ] || die "usage: gate-assessment <bead_id> <pr_number> <gates_json>"
  valid_pr "$3"; require_state "$2" ATTESTED; require_pr "$2" "$3"
  python3 - "$4" <<'PY' || die "invalid gates json"
import json, sys
g = json.loads(sys.argv[1])
keys = {"ci_green","no_conflicts","coderabbit","bugbot","comments_resolved","evidence_review","skeptic"}
assert set(g) == keys, f"gate keys must be exactly {sorted(keys)}"
assert all(v in ("green","red","unknown") for v in g.values()), "gate values must be green|red|unknown"
PY
  all_green="$(python3 -c 'import json,sys; g=json.loads(sys.argv[1]); print("true" if all(v=="green" for v in g.values()) else "false")' "$4")"
  # cooldown decision is the HARNESS's call, not the LLM's: ready iff the most
  # recent PRIOR assessment for this PR (before this emit) was also not-all-green
  prior_last="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$3[,}]" | tail -1 || true)"
  cooldown="false"
  if [ -n "$prior_last" ] && printf '%s' "$prior_last" | grep -q '"all_green": false'; then cooldown="true"; fi
  emit "$2" "$(get_field "$2" attempt)" ATTESTED GATE_ASSESSMENT '{}' "{\"pr_number\":$3,\"gates\":$4,\"all_green\":$all_green}"
  echo "$all_green"
  echo "cooldown_ready=$cooldown"
  ;;

prev-gate-assessment) # prev-gate-assessment <pr_number> — prints previous (second-to-last) assessment or empty
  [ $# -eq 2 ] || die "usage: prev-gate-assessment <pr_number>"
  valid_pr "$2"
  m="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$2[,}]" || true)"
  if [ "$(printf '%s\n' "$m" | grep -c .)" -ge 2 ]; then printf '%s\n' "$m" | tail -2 | head -1; fi
  ;;

ready) # ready <bead_id> <pr_number> — terminal transition; verifier stops driving this bead
  [ $# -eq 3 ] || die "usage: ready <bead_id> <pr_number>"
  valid_pr "$3"; require_state "$2" ATTESTED; require_pr "$2" "$3"
  # terminal transition requires an evidenced all-green assessment for THIS pr
  last_ga="$(grep '"eventType": *"GATE_ASSESSMENT"' "$LOG" 2>/dev/null | grep -E "\"pr_number\": *$3[,}]" | tail -1 || true)"
  printf '%s' "$last_ga" | grep -q '"all_green": *true' || die "ready refused: no all_green=true GATE_ASSESSMENT for pr $3"
  sql "UPDATE bead_overlay SET state='READY', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
  emit "$2" "$(get_field "$2" attempt)" READY READY_FOR_MERGE '{}' "{\"pr_number\":$3}"
  echo "ok"
  ;;

reroll-verdict) # reroll-verdict <bead_id> <pr_number> <in_place_fixable|reroll_worthy> <rationale>
  [ $# -eq 5 ] || die "usage: reroll-verdict <bead_id> <pr_number> <verdict> <rationale>"
  case "$4" in in_place_fixable|reroll_worthy) ;; *) die "invalid verdict: $4";; esac
  valid_pr "$3"; require_state "$2" ATTESTED; require_pr "$2" "$3"
  rat_json="$(printf '%s' "$5" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')"
  emit "$2" "$(get_field "$2" attempt)" ATTESTED REROLL_VERDICT_RECORDED '{}' "{\"pr_number\":$3,\"verdict\":\"$4\",\"rationale\":$rat_json}"
  if [ "$4" = "reroll_worthy" ]; then  # stage 1: record + park, never execute
    sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
    emit "$2" "$(get_field "$2" attempt)" HUMAN_HELD PARKED_HUMAN_HELD '{}' '{"reason":"reroll_worthy_stage1_disabled"}'
  fi
  echo "ok"
  ;;

park) # park <bead_id> <reason>
  [ $# -eq 3 ] || die "usage: park <bead_id> <reason>"
  valid_bead_id "$2"
  sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
  emit "$2" "$(get_field "$2" attempt)" HUMAN_HELD PARKED_HUMAN_HELD '{}' "{\"reason\":$(printf '%s' "$3" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}"
  echo "ok"
  ;;

bead-closed-check) # bead-closed-check <bead_id> — detect a bead closed underneath a DISPATCHED/ATTESTED row
  [ $# -eq 2 ] || die "usage: bead-closed-check <bead_id>"
  bead_id="$2"
  [ -n "$(get_field "$bead_id" bead_id)" ] || die "unknown bead_id (no overlay row): $bead_id"
  br_json="$("$BR_BIN" show "$bead_id" --json 2>/dev/null)" || die "br show failed for $bead_id"
  status="$(printf '%s' "$br_json" | python3 -c 'import json,sys
d = json.load(sys.stdin)
if not d:
    raise SystemExit("br show returned no records for bead")
print(d[0]["status"])' 2>&1)" || die "could not parse br show --json output for $bead_id: $status"
  cur_state="$(get_field "$bead_id" state)"
  if [ "$status" != "closed" ]; then
    echo "open"
  elif [ "$cur_state" = "DISPATCHED" ] || [ "$cur_state" = "ATTESTED" ]; then
    branch="$(get_field "$bead_id" branch)"
    pr_number="$(get_field "$bead_id" pr_number)"
    sql "UPDATE bead_overlay SET state='HUMAN_HELD', updated_at='$(now)' WHERE bead_id='$(q "$bead_id")';"
    ctx="$(python3 -c 'import json,sys; print(json.dumps({"reason":"bead_closed_underneath","prior_state":sys.argv[1],"branch":(sys.argv[2] or None),"pr_number":(int(sys.argv[3]) if sys.argv[3] not in ("", "None") else None)}))' "$cur_state" "$branch" "$pr_number")"
    emit "$bead_id" "$(get_field "$bead_id" attempt)" HUMAN_HELD PARKED_HUMAN_HELD '{}' "$ctx"
    echo "parked"
  else
    echo "already_terminal"
  fi
  ;;

tick-summary) # tick-summary <role>
  [ $# -eq 2 ] || die "usage: tick-summary <coder|verifier>"
  counts="$(sql -json "SELECT state, count(*) AS n FROM bead_overlay GROUP BY state;" | python3 -c 'import json,sys; d=json.load(sys.stdin) if (s:=sys.stdin) else []; print(json.dumps({r["state"].lower(): r["n"] for r in d}))' 2>/dev/null || echo '{}')"
  emit "tick" 0 QUEUED TICK "$counts" "{\"role\":\"$2\"}"
  echo "ok"
  ;;

list) # list <STATE> — bead_id|pr_number|branch|attempt rows
  [ $# -eq 2 ] || die "usage: list <STATE>"
  valid_state "$2"
  sql -json "SELECT bead_id, pr_number, branch, attempt, autonomy_secs FROM bead_overlay WHERE state='$2';"
  ;;

*)
  die "unknown subcommand: ${1:-<none>}. Valid: init intake-upsert route-record capacity dispatch-record pr-opened autonomy-tick gate-assessment prev-gate-assessment ready reroll-verdict park bead-closed-check tick-summary list"
  ;;
esac
