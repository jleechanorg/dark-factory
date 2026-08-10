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
#   rollback-dispatched           — DISPATCHED → QUEUED for orphaned async-spawns
#   redrive-pr <id> <pr> <branch> — re-QUEUE existing PR for re-attempt
#   list <STATE>                  — print bead_overlay rows for state
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BR_DB="${BR_DB:-$ROOT/.beads/beads.db}"
br() { command br --db "$BR_DB" "$@"; }
DB="${AFD_DB:-$HOME/.dark-factory/daemon-cxdb.sqlite}"
LOG="${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
DAEMON_BIN="${AFD_DAEMON_BIN:-$ROOT/daemon/target/release/daemon}"
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
  if [[ "$1" =~ ^[A-Za-z0-9._/+-]+$ ]] && [[ ! "$1" =~ ^factory/ ]] && [[ ! "$1" =~ ^refs/ ]] && [[ "$1" != "HEAD" ]]; then
    return 0
  fi
  die_code $EX_VALID_INPUT "branch must match factory/<bead_id>-r<n> OR existing-PR branch name: $1"
}
valid_pr() {
  [[ "$1" =~ ^[0-9]+$ ]] || die_code $EX_VALID_INPUT "pr_number must be numeric: $1"
  [ "${#1}" -le 10 ] || die_code $EX_VALID_INPUT "pr_number too large (max 10 digits): $1"
  # Arithmetic (not string) comparison: "00"/"000" pass the digit regex and
  # length bound but are string-unequal to "0", so a `[ "$1" != "0" ]` check
  # let them through and they normalized to pr_number=0 downstream in
  # SQLite. 10#$1 forces base-10 interpretation so a leading zero can't be
  # misread as octal; the length bound above already rules out
  # arithmetic overflow here.
  [ "$((10#$1))" -ge 1 ] || die_code $EX_VALID_INPUT "pr_number must be >= 1: $1"
}
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
  # CR-6: probe row count FIRST so a missing bead returns rc=8 (EX_NOT_FOUND)
  # instead of the rc=5 (EX_REQUIRE_STATE) it would produce if we let `cur` be
  # empty and matched the case fallback. get_field returns empty for both
  # "no row" and "wrong state" — only an existence check distinguishes them.
  if ! exists="$(sql "SELECT count(*) FROM bead_overlay WHERE bead_id='$(q "$2")';")" 2>/dev/null; then
    die_code $EX_IO "existence lookup failed for $2"
  fi
  if [ "$exists" = "0" ]; then
    die_code $EX_NOT_FOUND "bead has no overlay row: $2"
  fi
  if ! cur="$(get_field "$2" state)" 2>/dev/null; then
    die_code $EX_IO "state lookup failed for $2"
  fi
  case " QUEUED " in *" $cur "*) ;; *) die_code $EX_REQUIRE_STATE "bead in state '$cur', expected one of: QUEUED";; esac
  # CR-7 (capacity leg): "$0 capacity" recurses into a fresh invocation of this
  # same script, which also runs under `set -euo pipefail`. If ITS internal
  # `sql` call fails (missing table, corrupt DB), that subprocess exits with
  # sqlite3's raw error code — and a bare assignment here would let that raw
  # code propagate straight out of THIS process too, bypassing the structured
  # EX_IO=9 contract. Capture explicitly, same pattern as the owner lookup below.
  set +e
  cur_cap="$("$0" capacity)"
  cap_rc=$?
  set -e
  if [ "$cap_rc" -ne 0 ]; then
    die_code $EX_IO "capacity lookup failed (rc=$cap_rc)"
  fi
  [ "$cur_cap" -gt 0 ] || die_code $EX_OVER_CAP "over capacity — dispatch refused (capacity=$cur_cap)"
  # CR-7: capture sql exit code so missing tables / corrupt DBs surface as EX_IO
  # instead of silently passing through. set -e does NOT propagate rc from
  # command substitutions that are immediately consumed (e.g., `if [ -n ]`),
  # so the rc must be checked explicitly.
  set +e
  owner="$(sql "SELECT bead_id FROM branch_registry WHERE branch='$(q "$3")';")"
  sql_rc=$?
  set -e
  if [ "$sql_rc" -ne 0 ]; then
    die_code $EX_IO "branch_registry read failed (rc=$sql_rc) for $3"
  fi
  if [ -n "$owner" ] && [ "$owner" != "$2" ]; then
    die_code $EX_BRANCH_CONFLICT "branch $3 already registered to $owner"
  fi
  # CR-7: capture INSERT/UPDATE errors so missing tables / corrupt DBs surface
  # as EX_IO instead of silently leaving state inconsistent. The `sql` helper
  # runs sqlite3 directly; without an explicit error capture, sqlite errors go
  # to stderr and the script exits 0.
  sql_err="$(mktemp -t factory_overlay_err.XXXXXX)"
  set +e
  sql "INSERT INTO branch_registry (branch,bead_id,created_at)
       VALUES ('$(q "$3")','$(q "$2")','$(now)') ON CONFLICT(branch) DO NOTHING;
       UPDATE bead_overlay SET state='DISPATCHED', branch='$(q "$3")', updated_at='$(now)'
       WHERE bead_id='$(q "$2")';" 2>"$sql_err"
  sql_rc=$?
  set -e
  if [ "$sql_rc" -ne 0 ]; then
    err_msg="$(cat "$sql_err" 2>/dev/null | tr -d '\n' | head -c 200)"
    rm -f "$sql_err"
    die_code $EX_IO "dispatch-record write failed (rc=$sql_rc) for $2: $err_msg"
  fi
  rm -f "$sql_err"
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
# 8 required gates (canonical source: daemon/src/verifier.rs::GateName).
# code_standards and zfc are optional advisory keys — accepted but not required.
# See bead jleechan-1gft for tracking the optional expansion to real automated gates.
# Gate 8 (`vacuous_red_green`, bead jleechan-ijod / issue #387) was added in
# PR #389 / r5 commit 175c6ad — runtime vacuous-test detector verdict
# propagated from PrEvidence.vacuous_red_green.
#
# Verdict value shape (jleechan-240 additive expansion):
#   * String:  "pass" | "warn" | "fail" | "unknown"
#   * Object:  {"verdict": "...", "evidence": <path:line:msg list>}
# Legacy aliases "green"/"red"/"unknown" map to "pass"/"fail"/"unknown".
# Unknown verdict tokens are rejected (no keyword routing — only the
# invoking model decides pass|warn|fail).
REQUIRED_KEYS = {"ci_green","no_conflicts","coderabbit","bugbot","comments_resolved","evidence_review","skeptic","vacuous_red_green"}
OPTIONAL_KEYS = {"code_standards","zfc"}
ALIAS = {"green":"pass","red":"fail","warn":"warn","unknown":"unknown","pass":"pass","fail":"fail"}
VALID = {"pass","warn","fail","unknown"}

def normalize(v, key):
    if isinstance(v, str):
        if v not in ALIAS:
            raise AssertionError(f"gate[{key}] verdict must be pass|warn|fail|unknown (or alias green|red|unknown); got {v!r}")
        return ALIAS[v]
    if isinstance(v, dict):
        unknown_keys = set(v.keys()) - {"verdict","evidence"}
        if unknown_keys:
            raise AssertionError(f"gate[{key}] object may only have 'verdict'+'evidence'; got {sorted(v.keys())}")
        if "verdict" not in v:
            raise AssertionError(f"gate[{key}] object must contain 'verdict'")
        verdict = v["verdict"]
        if not isinstance(verdict, str) or verdict not in ALIAS:
            raise AssertionError(f"gate[{key}].verdict must be pass|warn|fail|unknown; got {verdict!r}")
        return ALIAS[verdict]
    raise AssertionError(f"gate[{key}] value must be string or object; got {type(v).__name__}")

missing = REQUIRED_KEYS - set(g.keys())
extra = set(g.keys()) - REQUIRED_KEYS - OPTIONAL_KEYS
if missing:
    raise AssertionError(f"missing required gates: {sorted(missing)}")
if extra:
    raise AssertionError(f"unknown gates (not in REQUIRED or OPTIONAL): {sorted(extra)}")
verdicts = {k: normalize(v, k) for k, v in g.items()}
PYGA
  all_green="$(python3 - "$4" <<'PYGB'
import json, sys
g = json.loads(sys.argv[1])
ALIAS = {"green":"pass","red":"fail","warn":"warn","unknown":"unknown","pass":"pass","fail":"fail"}
def verdict(v):
    if isinstance(v, str):
        return ALIAS.get(v, v)
    if isinstance(v, dict):
        return ALIAS.get(v.get("verdict",""), v.get("verdict",""))
    return v
# all_green=true iff every gate returned a positive verdict ("pass" or
# "warn"). "fail" routes through reroll-verdict -> HUMAN_HELD ->
# recover-held -> QUEUED (the bounded fix loop). "unknown" means the
# verifier hasn't gathered evidence yet and must wait for the next tick;
# treating unknown as green would let stale beads race to READY without
# independent review.
print("true" if all(verdict(v) in ("pass","warn") for v in g.values()) else "false")
PYGB
)"
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
    sql "UPDATE bead_overlay SET state='HUMAN_HELD', session_id=NULL, park_reason='gate assessment not all-green (stage 1: recorded, not executed)', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
    emit "$2" "$cur_attempt" HUMAN_HELD PARKED_HUMAN_HELD '{"reason":"reroll_worthy_stage1_disabled"}'
  fi
  echo "ok"
  ;;

park)
  [ $# -eq 3 ] || die "usage: park <bead_id> <reason>"
  valid_bead_id "$2"
  cur_attempt="$(get_field "$2" attempt)"
  [[ "$cur_attempt" =~ ^[0-9]+$ ]] || cur_attempt=1
  sql "UPDATE bead_overlay SET state='HUMAN_HELD', park_reason='$(q "$3")', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
  emit "$2" "$cur_attempt" HUMAN_HELD PARKED_HUMAN_HELD "{\"reason\":$(js "$3")}"
  echo "ok"
  ;;

park-duplicate)
  [ $# -eq 3 ] || die "usage: park-duplicate <bead_id> <reason>"
  valid_bead_id "$2"
  cur_attempt="$(get_field "$2" attempt)"
  [[ "$cur_attempt" =~ ^[0-9]+$ ]] || cur_attempt=1
  sql "UPDATE bead_overlay SET state='HUMAN_HELD', park_reason='$(q "$3")', updated_at='$(now)' WHERE bead_id='$(q "$2")';"
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
# jleechan-240 align guard with the verdict vocabulary accepted by
# gate-assessment: block on fail (or the legacy "red" alias), including
# the structured {"verdict":"fail", "evidence":[...]} shape. Warn and
# unknown stay non-blocking per the documented no-red merge policy.
ALIAS={"green":"pass","red":"fail","yellow":"warn","warn":"warn","unknown":"unknown","pass":"pass","fail":"fail"}
def verdict(v):
    if isinstance(v, str):
        return ALIAS.get(v, v)
    if isinstance(v, dict):
        return ALIAS.get(v.get("verdict",""), v.get("verdict",""))
    return v
sys.exit(0 if not any(verdict(v) == "fail" for v in g.values()) else 1)'; then
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
  [ $# -eq 1 ] || die "usage: recover-held"
  [ -x "$DAEMON_BIN" ] || die_code $EX_IO \
    "canonical recovery binary unavailable: $DAEMON_BIN (refusing unsafe shell fallback)"
  "$DAEMON_BIN" recover-held --db "$DB" --telemetry-log "$LOG"
  ;;

unstick-dispatching)
  n="$(sql "UPDATE bead_overlay SET state='QUEUED', updated_at='$(now)' WHERE state='DISPATCHING'; SELECT changes();")"
  echo "unstuck=$n"
  ;;

rollback-dispatched)
  # Codex P1 finding on PR #193: factory-ao-remediate.sh (async mode) returns
  # success optimistically when the spawn is still pending. If the spawn then
  # fails AFTER the fast-fail window (slow internal error, daemon dies
  # mid-spawn), the bead is stranded as DISPATCHED with no AO session. This
  # subcommand reads each DISPATCHED bead's spawn-state file (written by the
  # detached background process in factory-ao-remediate.sh) and rolls the bead
  # back to QUEUED when the state file shows "fail:rc=N".
  #
  # State file path: $AFD_SPAWN_STATE_DIR/${bead_id}-${pr_number}.state
  # State values: "pending" (spawn still running), "ok" (success),
  #               "fail:rc=N" (failure with rc)
  SPAWN_STATE_DIR_ROLL="${AFD_SPAWN_STATE_DIR:-$HOME/Library/Application Support/dark-factory/spawns}"
  rolled=0
  while IFS='|' read -r rb_id rb_pr; do
    [ -n "$rb_id" ] || continue
    state_file="$SPAWN_STATE_DIR_ROLL/${rb_id}-${rb_pr}.state"
    [ -f "$state_file" ] || continue
    cur="$(cat "$state_file" 2>/dev/null || true)"
    case "$cur" in
      fail:*)
        sql "UPDATE bead_overlay SET state='QUEUED', updated_at='$(now)' WHERE bead_id='$(q "$rb_id")' AND state='DISPATCHED';"
        ctx="$(python3 -c 'import json,sys; v=sys.argv[1]; print(json.dumps({"pr_number":(int(v) if v not in ("","NULL") else None), "reason":"async_spawn_failed","state_file_value":v}))' "$rb_pr" "$cur")"
        emit "$rb_id" 0 QUEUED ROLLBACK_DISPATCHED "$ctx"
        rolled=$((rolled + 1))
        ;;
    esac
  done < <(sql -separator '|' "SELECT bead_id, coalesce(cast(pr_number as text),'') FROM bead_overlay WHERE state='DISPATCHED' AND pr_number IS NOT NULL;")
  echo "rolled=$rolled"
  ;;

redrive-pr)
  [ $# -eq 4 ] || die "usage: redrive-pr <bead_id> <pr_number> <branch>"
  valid_bead_id "$2"
  valid_pr "$3"
  valid_branch "$4"
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
  die "unknown: ${1:-}. Valid: init intake-upsert route-record capacity dispatch-record pr-opened autonomy-tick gate-assessment prev-gate-assessment ready reroll-verdict park park-duplicate bead-closed-check tick-summary recover-held unstick-dispatching rollback-dispatched redrive-pr list"
  ;;
esac
