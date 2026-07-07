#!/usr/bin/env bash
# Deterministic /af one tick: intake + recover + AO dispatch for drive-existing-pr beads.
# The tick ends with a READY scheduling step (ready-scheduler.sh) that closes
# Blocker #7 from docs/factory-goal-gap-review-2026-07-06.md WITHOUT taking
# the merge side effect: the 7-green pre-merge checks are not yet enforceable
# (gate 6 /er has no automated runner — bead jleechan-qqq still open), so we
# transition beads to READY only and leave the actual merge to a future
# authority that has the full 7-green evidence. READY scheduler is opt-out via
# READY_SCHEDULER_DISABLED=1 (mirrors cutover X7's single-writer pattern).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BR_DB="${BR_DB:-$ROOT/.beads/beads.db}"
br() { command br --db "$BR_DB" "$@"; }
O="$ROOT/daemon/factory-overlay.sh"
I="$ROOT/daemon/factory-intake-from-gh.sh"
R="$ROOT/daemon/factory-ao-remediate.sh"
G="$ROOT/daemon/scripts/ready-scheduler.sh"
DB="${AFD_DB:-$HOME/.dark-factory/daemon-cxdb.sqlite}"
LOG="${AFD_LOG:-$HOME/Library/Logs/dark-factory/daemon.jsonl}"
MAX_DISPATCH="${MAX_DISPATCH:-2}"
READY_SCHEDULER_DISABLED="${READY_SCHEDULER_DISABLED:-0}"

TARGET_PRS=""
args=("$@")
i=0
while [ "$i" -lt "${#args[@]}" ]; do
  if [ "${args[$i]}" = "--prs" ] && [ $((i + 1)) -lt "${#args[@]}" ]; then
    TARGET_PRS="${args[$((i + 1))]}"
    break
  fi
  i=$((i + 1))
done

cd "$ROOT"
"$O" init
"$O" unstick-dispatching
"$O" recover-held
for dup in jleechan-4uzw jleechan-bxjy jleechan-hslx jleechan-ccfin; do
  sqlite3 "$DB" "SELECT state FROM bead_overlay WHERE bead_id='$dup';" 2>/dev/null | rg -q . && \
    "$O" park-duplicate "$dup" "superseded-by-canonical-bead" 2>/dev/null || true
done
bash "$I"

if br show jleechan-7re5 >/dev/null 2>&1; then
  branch8189="$(gh pr view 8189 --repo jleechanorg/worldarchitect.ai --json headRefName -q .headRefName 2>/dev/null || true)"
  "$O" intake-upsert jleechan-7re5 "[worldai] Fix GHA ensurepip fallback PR #8189" >/dev/null || true
  if [ -n "$branch8189" ]; then
    "$O" redrive-pr jleechan-7re5 8189 "$branch8189" >/dev/null || true
  fi
fi

AO="$(bash "$ROOT/daemon/factory-ao-bin.sh" 2>/dev/null || true)"
AO_CAP="${AO_MAX_CONCURRENT_SESSIONS:-30}"
if [ -n "$AO" ]; then
  if "$AO" session ls --json >/dev/null 2>&1; then
    ao_active="$("$AO" session ls --json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(1 for s in d.get("data",[]) if not s.get("isTerminated")))' 2>/dev/null || echo 0)"
  else
    ao_active="$("$AO" session ls -p worldarchitect 2>/dev/null | rg -c '\[(spawning|running|active|working|pr_open)\]' || echo 0)"
  fi
  if [ "${ao_active:-0}" -ge "$AO_CAP" ]; then
    echo "[af] AO cap: ${ao_active} active >= ${AO_CAP} — skipping dispatch (intake done)" >&2
    MAX_DISPATCH=0
  fi
fi

pr_sql_filter=""
if [ -n "$TARGET_PRS" ]; then
  pr_sql_filter="AND pr_number IN ($(echo "$TARGET_PRS" | tr ',' ' ' | awk '{for(i=1;i<=NF;i++) printf "%s%s", (i>1?",":""), $i}'))"
fi

bead_filter=""
if [ -n "${AFD_BEAD_FILTER:-}" ]; then
  bead_filter_sql="$(echo "$AFD_BEAD_FILTER" | tr ' ' ',' | awk 'BEGIN{FS=OFS=","} {for(i=1;i<=NF;i++) printf "%s\047%s\047", (i==1?"":","), $i}')"
  bead_filter="AND bead_id IN ($bead_filter_sql)"
else
  bead_filter="AND bead_id IN ('jleechan-9byt.4','jleechan-93ft','jleechan-7re5')"
fi

dispatched=0
ERR_TMP="$(mktemp -t af_dispatch_err.XXXXXX)"
trap 'rm -f "$ERR_TMP"' EXIT
while IFS=$'\t' read -r bead_id pr branch; do
  [ -n "$bead_id" ] || continue
  [ "$dispatched" -ge "$MAX_DISPATCH" ] && break
  if [ -n "$AO" ] && "$AO" session ls -p worldarchitect 2>/dev/null | rg "pulls/${pr}\\b" | rg -q '\[(spawning|running|active|working|pr_open)\]'; then
    echo "[af] skip $bead_id PR #$pr (active session exists)" >&2
    continue
  fi
  echo "[af] remediate $bead_id PR #$pr"
  if bash "$R" "$bead_id" "$pr" 2>&1; then
    cur_state="$(sqlite3 "$DB" "SELECT state FROM bead_overlay WHERE bead_id='$(printf "%s" "$bead_id" | sed "s/'/''/g")';" 2>/dev/null || true)"
    if [ "$cur_state" = "QUEUED" ]; then
      if [ -n "$branch" ]; then
        "$O" route-record "$bead_id" STANDARD_PATH "drive-existing-pr" 2>/dev/null || true
      fi
      if "$O" dispatch-record "$bead_id" "$branch" 2>"$ERR_TMP"; then
        :
      else
        err="$(cat "$ERR_TMP" 2>/dev/null || true)"
        case "$err" in
          *over\ capacity*)
            cur_cap="$("$O" capacity 2>/dev/null || echo 0)"
            echo "[af] over capacity — skip $bead_id (capacity=$cur_cap)" >&2
            ;;
          *already\ registered*)
            owner="$(printf '%s' "$err" | sed -n 's/.*already registered to //p')"
            echo "[af] branch conflict $branch owned by $owner — skip $bead_id" >&2
            ;;
          *expected\ one\ of:*)
            echo "[af] dispatch-record refused for $bead_id (state=$cur_state not QUEUED): $err" >&2
            ;;
          *)
            echo "[af] dispatch-record refused for $bead_id: $err" >&2
            ;;
        esac
        continue
      fi
    fi
    dispatched=$((dispatched + 1))
  else
    echo "[af] skip $bead_id (ao spawn failed)" >&2
  fi
done < <(sqlite3 "$DB" -separator $'\t' \
  "SELECT bead_id, pr_number, coalesce(branch,'') FROM bead_overlay
   WHERE state IN ('QUEUED','ATTESTED') AND pr_number IS NOT NULL
   $bead_filter
   $pr_sql_filter
   ORDER BY CASE bead_id WHEN 'jleechan-7re5' THEN 0 WHEN 'jleechan-93ft' THEN 1 WHEN 'jleechan-9byt.4' THEN 2 ELSE 9 END, updated_at LIMIT 10;")

echo "af_dispatched=$dispatched"
callpath run dark-factory ${1+"$@"} 2>/dev/null || true

# --- READY scheduling step ------------------------------------------------
# Block on jleechan-s3c / Blocker #7: gives the factory an automatic caller
# for the READY transition (no `gh pr merge`). Merge authority stays with
# the human operator until the 7-green gate is enforceable (jleechan-qqq).
# READY_SCHEDULER_DISABLED=1 silences the scheduler for the cutover X7
# single-writer pattern.
# ---------------------------------------------------------------------------
if [ -x "$G" ] && [ "$READY_SCHEDULER_DISABLED" != "1" ]; then
  echo "[af] ready scheduling step: ready-scheduler.sh (AFD_LOG=$LOG)"
  # Resolve the configured target repo and pass it explicitly so the
  # scheduler scans the right PRs (Codex P1 thread PRRT_kwDOSjv_9s6O0bY3).
  # Falls back to config/daemon.toml target_repo inside ready-scheduler.sh.
  TARGET_REPO="${READY_SCHEDULER_REPO:-}"
  if [ -z "$TARGET_REPO" ] && [ -f "$ROOT/config/daemon.toml" ]; then
    TARGET_REPO="$(awk -F'"' '/^target_repo[[:space:]]*=/ {print $2; exit}' "$ROOT/config/daemon.toml" 2>/dev/null || true)"
  fi
  AFD_LOG="$LOG" READY_SCHEDULER_REPO="$TARGET_REPO" bash "$G" 2>&1 || echo "[af] ready-scheduler.sh exited non-zero (continuing)"
else
  echo "[af] ready scheduling step: SKIPPED (READY_SCHEDULER_DISABLED=$READY_SCHEDULER_DISABLED scheduler_present=$([ -x "$G" ] && echo 1 || echo 0))"
fi
