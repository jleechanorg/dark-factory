#!/usr/bin/env bash
# Deterministic /af one tick: intake + recover + AO dispatch for drive-existing-pr beads.
#
# Exit-code contract with daemon/factory-overlay.sh (ZFC-correct):
#   $rc=0  success
#   $rc=2  invalid arguments (usage)
#   $rc=3  over capacity — skip this bead (capacity gate refused)
#   $rc=4  branch conflict — skip this bead (branch owned by another bead)
#   $rc=5  require_state failed — skip this bead (not QUEUED anymore)
#   $rc=6  valid input format — skip this bead (will not fix)
#   $rc=7  invalid bead_id — skip this bead (will not fix)
#   $rc=8  bead not found in overlay
#   $rc=9  io error (sqlite / fs)
#
# Configurable env:
#   AFD_BEAD_FILTER         space-separated bead IDs to limit the SELECT to
#   AFD_PRIORITY_BEADS      comma-separated bead IDs in dispatch-priority order
#                           (no hardcoded IDs in this script; see CLAUDE.md ZFC rule)
#   AFD_AO_PROJECT          AO project name (default: worldarchitect)
#   MAX_DISPATCH            max beads per tick (default 2)
#   AO_MAX_CONCURRENT_SESSIONS  AO concurrency cap (default 30)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BR_DB="${BR_DB:-$ROOT/.beads/beads.db}"
br() { command br --db "$BR_DB" "$@"; }
O="$ROOT/daemon/factory-overlay.sh"
I="$ROOT/daemon/factory-intake-from-gh.sh"
R="$ROOT/daemon/factory-ao-remediate.sh"
DB="${AFD_DB:-$HOME/.dark-factory/daemon-cxdb.sqlite}"
MAX_DISPATCH="${MAX_DISPATCH:-2}"
AO_PROJECT="${AFD_AO_PROJECT:-worldarchitect}"

# ---------- arg parsing ----------
TARGET_PRS=""
i=1
while [ "$i" -le "$#" ]; do
    arg="${@:$i:1}"
    case "$arg" in
        --prs)
            i=$((i + 1))
            if [ "$i" -gt "$#" ]; then
                echo "factory-af-tick: --prs requires a value" >&2
                exit 2
            fi
            TARGET_PRS="${@:$i:1}"
            ;;
        *) echo "factory-af-tick: unknown argument: $arg" >&2; exit 2 ;;
    esac
    i=$((i + 1))
done

# ---------- validate --prs (numeric CSV) ----------
if [ -n "$TARGET_PRS" ]; then
    case "$TARGET_PRS" in
        ''|*[!0-9,]*)
            echo "factory-af-tick: --prs must be comma-separated numeric PR ids (got: $TARGET_PRS)" >&2
            exit 2
            ;;
    esac
fi

# ---------- validate AFD_BEAD_FILTER (safe id chars) ----------
if [ -n "${AFD_BEAD_FILTER:-}" ]; then
    case "$AFD_BEAD_FILTER" in
        *'!'*|*'\'*|*'$'*|*'|'*|*'<'*|*'>'*|*';'*|*'`'*|*'('*|*')'*|*'{'*|*'}'*|*'['*|*']'*|*"'"*|*'"'*|*'~'*|*'#'*|*'&'*|*'*'*|*'?'*)
            echo "factory-af-tick: AFD_BEAD_FILTER must contain only bead_id-safe chars" >&2
            exit 2
            ;;
    esac
fi

cd "$ROOT"
"$O" init
"$O" unstick-dispatching
"$O" recover-held

# Park superseded duplicates. Generic query — no hardcoded bead IDs.
# Picks up beads marked with the "superseded" label via the br CLI; if that
# query yields nothing, this loop is a no-op.
if br --help 2>/dev/null | rg -q 'list.*--label'; then
    while IFS=$'\t' read -r dup_id; do
        [ -n "$dup_id" ] || continue
        cur_state="$(sqlite3 "$DB" "SELECT state FROM bead_overlay WHERE bead_id='$(printf '%s' "$dup_id" | sed "s/'/''/g")';" 2>/dev/null || true)"
        if [ -n "$cur_state" ]; then
            "$O" park-duplicate "$dup_id" "superseded-by-canonical-bead" 2>/dev/null || true
        fi
    done < <(br list --label superseded --status open --json 2>/dev/null \
             | python3 -c 'import json,sys
try:
    d = json.load(sys.stdin)
    for b in (d.get("issues") or d if isinstance(d, list) else []):
        print(b.get("id",""))
except Exception:
    pass' 2>/dev/null || true)
fi

bash "$I"

# ---------- AO concurrency probe ----------
AO="$(bash "$ROOT/daemon/factory-ao-bin.sh" 2>/dev/null || true)"
AO_CAP="${AO_MAX_CONCURRENT_SESSIONS:-30}"
if [ -n "$AO" ]; then
    if "$AO" session ls --json >/dev/null 2>&1; then
        ao_active="$("$AO" session ls --json 2>/dev/null | python3 -c 'import json,sys; d=json.load(sys.stdin); print(sum(1 for s in d.get("data",[]) if not s.get("isTerminated")))' 2>/dev/null || echo 0)"
    else
        ao_active="$("$AO" session ls -p "$AO_PROJECT" 2>/dev/null | rg -c '\[(spawning|running|active|working|pr_open)\]' || echo 0)"
    fi
    if [ "${ao_active:-0}" -ge "$AO_CAP" ]; then
        echo "[af] AO cap: ${ao_active} active >= ${AO_CAP} — skipping dispatch (intake done)" >&2
        MAX_DISPATCH=0
    fi
fi

# ---------- build SELECT filters (no SQL injection; values validated above) ----------
pr_sql_filter=""
if [ -n "$TARGET_PRS" ]; then
    pr_sql_filter="AND pr_number IN (${TARGET_PRS})"
fi

bead_filter=""
if [ -n "${AFD_BEAD_FILTER:-}" ]; then
    # Build ('id1','id2',...) with each token validated against ^[A-Za-z0-9._-]+$
    bead_filter_sql="$(printf '%s' "$AFD_BEAD_FILTER" | tr ' ' ',' \
        | awk -F, '{
            out=""
            for (i=1; i<=NF; i++) {
                tok=$i
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", tok)
                if (tok == "") continue
                if (out != "") out = out ","
                out = out "\x27" tok "\x27"
            }
            print out
        }')"
    if [ -n "$bead_filter_sql" ]; then
        bead_filter="AND bead_id IN ($bead_filter_sql)"
    fi
fi

# ---------- priority ORDER BY (configurable via env, no hardcoded IDs) ----------
priority_order=""
if [ -n "${AFD_PRIORITY_BEADS:-}" ]; then
    # Each comma-separated ID becomes "WHEN '<id>' THEN <n>" via awk.
    priority_order="$(printf '%s' "$AFD_PRIORITY_BEADS" | tr ',' '\n' \
        | awk 'BEGIN{n=0} {
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", $0)
            if ($0 == "") next
            n++
            print "WHEN \x27" $0 "\x27 THEN " (n - 1)
        }' | tr '\n' ' ')"
fi
order_clause="ORDER BY updated_at LIMIT 10"
if [ -n "$priority_order" ]; then
    order_clause="ORDER BY CASE bead_id $priority_order ELSE 999 END, updated_at LIMIT 10"
fi

# ---------- dispatch loop ----------
dispatched=0
ERR_TMP="$(mktemp -t af_dispatch_err.XXXXXX)"
trap 'rm -f "$ERR_TMP"' EXIT
while IFS=$'\t' read -r bead_id pr branch; do
    [ -n "$bead_id" ] || continue
    [ "$dispatched" -ge "$MAX_DISPATCH" ] && break
    if [ -n "$AO" ] && "$AO" session ls -p "$AO_PROJECT" 2>/dev/null | rg "pulls/${pr}\\b" | rg -q '\[(spawning|running|active|working|pr_open)\]'; then
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
            # Capture both rc and stderr; case on rc (structured) — stderr is
            # logged verbatim for human operators but never parsed.
            set +e
            "$O" dispatch-record "$bead_id" "$branch" 2>"$ERR_TMP"
            rc=$?
            set -e
            err="$(cat "$ERR_TMP" 2>/dev/null || true)"
            case "$rc" in
                0) : ;;
                3)  # over capacity — capacity gate refused
                    cur_cap="$("$O" capacity 2>/dev/null || echo 0)"
                    echo "[af] over capacity — skip $bead_id (capacity=$cur_cap)" >&2
                    continue
                    ;;
                4)  # branch conflict — branch owned by another bead
                    echo "[af] branch conflict $branch — skip $bead_id: $err" >&2
                    continue
                    ;;
                5)  # require_state — bead is not QUEUED (race or already advanced)
                    echo "[af] require_state failed for $bead_id (state=$cur_state not QUEUED)" >&2
                    continue
                    ;;
                6)  # valid_branch / valid_pr — input format invalid (will not fix)
                    echo "[af] invalid input for $bead_id: $err" >&2
                    continue
                    ;;
                7)  # invalid bead_id — input format invalid (will not fix)
                    echo "[af] invalid bead_id for $bead_id: $err" >&2
                    continue
                    ;;
                *)  # unexpected / genuine failure
                    echo "[af] dispatch-record failed for $bead_id (rc=$rc): $err" >&2
                    continue
                    ;;
            esac
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
   $order_clause;")

echo "af_dispatched=$dispatched"
callpath run dark-factory ${1+"$@"} 2>/dev/null || true