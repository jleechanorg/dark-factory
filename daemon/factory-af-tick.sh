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
#   $rc=10 checkout drift — refusing to tick (not on main / dirty / behind
#          origin/main). See "Gate 0" block below. Set AFD_SKIP_DRIFT_CHECK=1
#          to bypass for local/dev runs (never set on the production plist).
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
# CONFIG/TARGET_REPO mirror the sibling scripts' pattern (daemon/factory-overlay.sh,
# daemon/factory-intake-from-gh.sh) so the per-bead repo/project resolution below
# has the same config file and a default to fall back on when a bead has no
# target_repo of its own.
CONFIG="${CONFIG:-$ROOT/config/daemon.toml}"
[ -f "$CONFIG" ] || CONFIG="$ROOT/daemon/contracts/daemon.toml.example"
TARGET_REPO="${TARGET_REPO:-}"

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

# ---------- validate --prs (numeric CSV; strict regex rejects empty/trailing) ----------
if [ -n "$TARGET_PRS" ]; then
    case "$TARGET_PRS" in
        ''|*[!0-9,]*)
            echo "factory-af-tick: --prs must be comma-separated numeric PR ids (got: $TARGET_PRS)" >&2
            exit 2
            ;;
    esac
    # Reject empty tokens (",," or leading/trailing ",") — they would produce
    # invalid SQL like "IN (,1,2)" or "IN (1,2,)".
    case ",${TARGET_PRS}," in
        *,,*) echo "factory-af-tick: --prs has empty, leading, or trailing comma (got: $TARGET_PRS)" >&2; exit 2 ;;
    esac
fi

# ---------- validate AFD_BEAD_FILTER (strict allowlist, single bead_id only) ----------
# AFD_BEAD_FILTER holds ONE bead_id (matches the contract used by the dispatch
# loop). The allowlist mirrors factory-overlay.sh:valid_bead_id (^[A-Za-z0-9._-]+$).
# Spaces, commas, and other delimiters are NOT valid — use one filter per tick.
if [ -n "${AFD_BEAD_FILTER:-}" ]; then
    case "$AFD_BEAD_FILTER" in
        *' '*|*,*) echo "factory-af-tick: AFD_BEAD_FILTER must be a single bead_id (no spaces/commas): $AFD_BEAD_FILTER" >&2; exit 2 ;;
    esac
    case "$AFD_BEAD_FILTER" in
        *[!A-Za-z0-9._-]*)
            echo "factory-af-tick: AFD_BEAD_FILTER must match ^[A-Za-z0-9._-]+\$ (got: $AFD_BEAD_FILTER)" >&2
            exit 2
            ;;
    esac
fi

# ---------- validate AFD_PRIORITY_BEADS (CR-4: SQL injection guard) ----------
# Each comma-separated token MUST match the strict allowlist. Without this, a
# crafted bead_id like `x' UNION SELECT ... --` is interpolated unescaped into
# the CASE WHEN fragment below and executed by sqlite3.
if [ -n "${AFD_PRIORITY_BEADS:-}" ]; then
    bad_token="$(printf '%s' "$AFD_PRIORITY_BEADS" | tr ',' '\n' \
        | awk '{
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", $0)
            if ($0 == "") next
            if ($0 !~ /^[A-Za-z0-9._-]+$/) { print $0; fflush(); exit 0 }
        }')"
    if [ -n "$bad_token" ]; then
        echo "factory-af-tick: AFD_PRIORITY_BEADS has invalid token (must match ^[A-Za-z0-9._-]+\$): $bad_token" >&2
        exit 2
    fi
fi

cd "$ROOT"

# ---------- Gate 0: refuse to tick on a drifted checkout ----------
# Bead jleechan-vxs8: the launchd daemon executes whatever branch happens to
# be checked out in $ROOT (normally ~/projects/dark-factory, a dev working
# tree shared with interactive sessions). On 2026-07-11 the tree sat on a
# feature branch that crashed every tick until another session switched
# branches out from under the daemon — neither state was a deliberate
# deploy, and the daemon silently ran whichever code happened to be on disk.
#
# Mirrors the ez-gh-actions Gate 0 SHA-pinning pattern: rather than adding a
# separate deploy-owned checkout (more moving parts, needs its own update
# step + install-launchagents.sh rewiring), the tick script itself refuses
# to do dispatch work when the checkout has drifted from origin/main or has
# uncommitted changes. A drifted checkout fails LOUD (non-zero exit, clear
# log line) instead of silently running unaudited code.
#
# Opt-out for local/dev/test runs (coder sessions iterating on a feature
# branch must be able to invoke this script directly without tripping the
# gate): set AFD_SKIP_DRIFT_CHECK=1. The installed launchd plist never sets
# this — production ticks always run the check.
if [ "${AFD_SKIP_DRIFT_CHECK:-0}" != "1" ]; then
    current_branch="$(git branch --show-current 2>/dev/null || true)"
    if [ "$current_branch" != "main" ]; then
        echo "factory-af-tick: REFUSING TICK — checkout at $ROOT is on branch '${current_branch:-<detached HEAD>}', not main. The daemon must run from main; switch back with 'git checkout main' or set AFD_SKIP_DRIFT_CHECK=1 for local dev runs." >&2
        exit 10
    fi

    if ! git diff --quiet HEAD -- 2>/dev/null || ! git diff --quiet --cached HEAD -- 2>/dev/null; then
        echo "factory-af-tick: REFUSING TICK — checkout at $ROOT has uncommitted changes on main. Run 'git status' and clean the tree (stash/reset) before the daemon can tick again." >&2
        exit 10
    fi

    # Compare local HEAD against origin/main. Best-effort fetch: if the
    # network/GH auth is unavailable this tick, don't hard-fail on the fetch
    # itself (that would turn a transient network blip into a dispatch
    # outage) — only fail when we DO have fresh remote data and it disagrees
    # with local HEAD.
    if git fetch origin main --quiet 2>/dev/null; then
        local_sha="$(git rev-parse HEAD 2>/dev/null || true)"
        remote_sha="$(git rev-parse refs/remotes/origin/main 2>/dev/null || true)"
        if [ -n "$local_sha" ] && [ -n "$remote_sha" ] && [ "$local_sha" != "$remote_sha" ]; then
            echo "factory-af-tick: REFUSING TICK — checkout at $ROOT (HEAD ${local_sha:0:9}) has drifted from origin/main (${remote_sha:0:9}). Run 'git pull --ff-only' to resync before the daemon can tick again." >&2
            exit 10
        fi
    fi
fi

"$O" init
"$O" unstick-dispatching
"$O" recover-held
# Roll back DISPATCHED beads whose async spawn failed (state file "fail:rc=N").
# Closes the Codex P1 loop on PR #193: async-spawn can succeed at the wrapper
# level (fast-fail window passed) but fail later; the state file records the
# final outcome and the next tick's `rollback-dispatched` rolls those beads
# back to QUEUED for retry.
"$O" rollback-dispatched

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
    # CX-1: br list --json may return either {"issues": [...]} (dict shape) or
    # a top-level list. The old expression `(d.get("issues") or d if isinstance(d, list))`
    # raised AttributeError on list (no .get) and returned [] on dict — silently
    # skipping superseded beads and leaving them dispatchable. Normalize: if dict,
    # extract the issues field; if list, pass through; otherwise empty.
    if isinstance(d, dict):
        issues = d.get("issues", [])
    elif isinstance(d, list):
        issues = d
    else:
        issues = []
    for b in issues:
        if isinstance(b, dict):
            print(b.get("id",""))
except Exception:
    pass' 2>/dev/null || true)
fi

bash "$I"

# ---------- AO concurrency probe ----------
# Session cache: query AO once per project and cache the output so per-bead
# session dedup (below) does NOT re-query AO for every bead. Fixes issue #270:
# the tick loop was serializing behind 1-2 `ao session ls` calls per bead,
# blocking dispatch for 40-110s per lane.
#
# Cache directory is initialized BEFORE the function definitions so it exists
# under `set -u` (skeptic fix #1: AFD_SESSION_CACHE_DIR init).
AFD_SESSION_CACHE_DIR="${AFD_SESSION_CACHE_DIR:-$(mktemp -d -t af_sessions.XXXXXX)}"
trap 'rm -rf "${AFD_SESSION_CACHE_DIR}"' EXIT

ao_session_cache() {
  local proj="$1"
  # Validate project name against strict allowlist to prevent path traversal
  # (skeptic fix #9: validate ao_project cache names).
  if [[ ! "$proj" =~ ^[A-Za-z0-9._-]+$ ]]; then
    echo "CACHE_ERROR:invalid_project_name=${proj}"
    return 0
  fi
  local cache_file="${AFD_SESSION_CACHE_DIR}/${proj}.txt"
  if [ -f "$cache_file" ]; then
    cat "$cache_file"
    return
  fi
  # Timeout-bounded: `ao session ls` can hang or be slow. Cap at 10s per project.
  if [ -z "$AO" ]; then
    touch "$cache_file"
    return 0
  fi
  if timeout 10 "$AO" session ls -p "$proj" 2>/dev/null > "$cache_file"; then
    :
  else
    # Fail closed: write a sentinel so callers do NOT treat an empty cache
    # as "no active sessions" and proceed to dispatch. (skeptic fix #2)
    echo "CACHE_ERROR:ao_query_failed" > "$cache_file"
  fi
  [ -s "$cache_file" ] || echo "CACHE_ERROR:empty_response" > "$cache_file"
  cat "$cache_file"
}

ao_active_sessions() {
  # Count non-terminated sessions across cached project data.
  # Returns: integer count of active sessions, or -1 on cache error (fail closed).
  local proj="$1"
  local raw
  raw="$(ao_session_cache "$proj" 2>/dev/null || true)"
  if [ -z "$raw" ]; then
    echo 0
    return
  fi
  # Fail closed: cache errors propagate as -1 so the capacity gate refuses dispatch.
  case "$raw" in
    CACHE_ERROR:*) echo -1; return ;;
  esac
  if [ "$raw" = "[]" ]; then
    echo 0
    return
  fi
  # skeptic fix #9: localize variable name (was 'total', now 'active_count').
  local active_count
  active_count="$(echo "$raw" | rg -c '\[(spawning|running|active|working|pr_open)\]' 2>/dev/null || echo 0)"
  case "$active_count" in
    ''|*[!0-9]*) echo 0 ;;
    *) echo "$active_count" ;;
  esac
}

ao_session_exists() {
  # Return 0 if an active session exists for the PR in the given project.
  # Return 1 on session-not-found OR on cache error (fail closed).
  local proj="$1" pr="$2"
  local raw
  raw="$(ao_session_cache "$proj" 2>/dev/null)"
  case "$raw" in
    CACHE_ERROR:*) return 1 ;;
  esac
  echo "$raw" | rg "pulls/${pr}\b" | rg -q '\[(spawning|running|active|working|pr_open)\]' 2>/dev/null
}

AO="$(bash "$ROOT/daemon/factory-ao-bin.sh" 2>/dev/null || true)"
AO_CAP="${AO_MAX_CONCURRENT_SESSIONS:-30}"
# Validate AO_PROJECT is non-empty + looks like a project name. Defensive: an
# empty / malformed project would cause `session ls -p ""` to dump all projects
# and inflate the active count, falsely tripping AO_MAX_CONCURRENT_SESSIONS.
if [ -z "${AO_PROJECT:-}" ] || [[ ! "$AO_PROJECT" =~ ^[A-Za-z0-9._-]+$ ]]; then
    echo "[af] WARN: AFD_AO_PROJECT='${AO_PROJECT:-}' is empty or malformed; defaulting to 'worldarchitect'" >&2
    AO_PROJECT="worldarchitect"
fi
if [ -n "$AO" ]; then
    # Use cached session data. Pre-populate the cache with the default project
    # so the per-bead loop doesn't re-query AO.
    # Whole-tick bound (skeptic fix #5): the first cache fill per unique project
    # costs up to 10s (timeout on `ao session ls`). With the default config
    # mapping all repos to a single project, this is at most 10s per tick.
    # Each dispensed bead adds AFD_ASYNC_WAIT_SEC (5s) for fast-fail poll.
    # Total dispatch-lane bound per tick: 10s + (MAX_DISPATCH * 5s).
    ao_session_cache "$AO_PROJECT" > /dev/null 2>&1
    ao_active="$(ao_active_sessions "$AO_PROJECT")"
    case "$ao_active" in
        -1)
            # Fail closed: AO session count is unknowable (cache error). Refuse
            # dispatch rather than proceeding blindly. (skeptic fix #2)
            echo "[af] AO session cache error — refusing dispatch (fail closed)" >&2
            MAX_DISPATCH=0
            ;;
        *)
            if [ "${ao_active:-0}" -ge "$AO_CAP" ]; then
                echo "[af] AO cap: ${ao_active} active >= ${AO_CAP} — skipping dispatch (intake done)" >&2
                MAX_DISPATCH=0
            fi
            ;;
    esac
fi

# ---------- build SELECT filters (no SQL injection; values validated above) ----------
pr_sql_filter=""
if [ -n "$TARGET_PRS" ]; then
    pr_sql_filter="AND pr_number IN (${TARGET_PRS})"
fi

bead_filter=""
if [ -n "${AFD_BEAD_FILTER:-}" ]; then
    # AFD_BEAD_FILTER holds a SINGLE validated bead_id (allowlist enforced above).
    # No CSV / whitespace handling — direct interpolation with single-quote escape.
    bid="$(printf '%s' "$AFD_BEAD_FILTER" | sed "s/'/''/g")"
    bead_filter="AND bead_id = '${bid}'"
fi

# ---------- priority ORDER BY (configurable via env, no hardcoded IDs) ----------
# CR-4 SQL injection guard runs above (line 80ish) — each token has been
# validated against `^[A-Za-z0-9._-]+$` before reaching here.
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
trap 'rm -f "$ERR_TMP"; rm -rf "${AFD_SESSION_CACHE_DIR}"' EXIT
while IFS=$'\t' read -r bead_id pr branch bead_repo; do
    [ -n "$bead_id" ] || continue
    [ "$dispatched" -ge "$MAX_DISPATCH" ] && break

    # Resolve target repo (default to global TARGET_REPO if empty). Fail closed
    # (skip, don't guess) when neither is set — see bead jleechan-gvdw: a
    # hardcoded fallback here recreates the same-number cross-repo claim risk
    # this per-bead repo resolution exists to prevent.
    repo="${bead_repo:-${TARGET_REPO:-}}"
    if [ -z "$repo" ]; then
        echo "[af] skip $bead_id: no repo mapping (fail-closed, no bead_repo or TARGET_REPO)" >&2
        continue
    fi

    # Resolve AO project for this repo from config
    proj="$(python3 - "$CONFIG" "$repo" <<'PY'
import sys, toml
config_path = sys.argv[1]
target_repo = sys.argv[2]
try:
    cfg = toml.load(config_path)
except Exception:
    cfg = {}

# 1. Look in [repos]
repos = cfg.get("repos", {})
if target_repo in repos:
    print(repos[target_repo].get("ao_project", ""))
    sys.exit(0)

# 2. Compare to global target_repo
global_target = cfg.get("target_repo")
if target_repo == global_target:
    ao_project = cfg.get("ao_project")
    if ao_project:
        print(ao_project)
        sys.exit(0)
    # Derivation fallback
    project = target_repo.split('/')[-1]
    if project == "worldarchitect.ai":
        project = "worldarchitect"
    print(project)
    sys.exit(0)

# Unmapped
print("")
PY
)"

    if [ -z "$proj" ]; then
        echo "[af] fail closed: target repo '$repo' has no matching configured AO project. Parking bead $bead_id." >&2
        "$O" park "$bead_id" "unmapped_target_repo" >/dev/null || true
        continue
    fi

    # Pre-populate the session cache for this project so the dedup check
    # does NOT issue a live AO query per bead.
    ao_session_cache "$proj" > /dev/null 2>&1

    # Per-bead fail closed: if the project's session cache is errored, skip
    # this bead rather than dispatching blindly. (skeptic fix #2)
    if [ -f "${AFD_SESSION_CACHE_DIR}/${proj}.txt" ]; then
        if rg -q '^CACHE_ERROR:' "${AFD_SESSION_CACHE_DIR}/${proj}.txt" 2>/dev/null; then
            echo "[af] skip $bead_id: AO session cache error for project $proj (fail closed)" >&2
            continue
        fi
    fi

    if [ -n "$AO" ] && ao_session_exists "$proj" "$pr"; then
        echo "[af] skip $bead_id PR #$pr (active session exists in project $proj)" >&2
        continue
    fi
    echo "[af] remediate $bead_id PR #$pr on $repo in project $proj"
    # CX-2: thread proj and repo through to factory-ao-remediate.sh so the spawned
    # session lives in the same AO project.
    # skeptic fix #3: DO NOT skip fast-fail poll (no AFD_SKIP_FAST_FAIL_POLL=1).
    # The fast-fail window (AFD_ASYNC_WAIT_SEC, default 5s) catches immediate
    # spawn failures (auth/project errors). Skipping it causes the script to
    # return 0 immediately, then dispatch-record marks the bead DISPATCHED,
    # stranding it if the spawn already failed. The bead would remain stranded
    # until the next tick's rollback-dispatched runs — up to 240s of dead time.
    if bash "$R" "$bead_id" "$pr" "$repo" "$proj" 2>&1; then
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
                9)  # EX_IO — sqlite / fs failure. CR-5: hard-fail the tick so
                    # the IO error is not silently swallowed by the generic
                    # 'continue' branch. The overlay returned a structured code
                    # specifically because it could not write — continuing would
                    # mask real disk/db problems and re-dispatch the same bead.
                    echo "[af] dispatch-record EX_IO for $bead_id (rc=9): $err" >&2
                    exit 9
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
  "SELECT bead_id, pr_number, coalesce(branch,''), coalesce(target_repo,'') FROM bead_overlay
   WHERE state IN ('QUEUED','ATTESTED') AND pr_number IS NOT NULL
   $bead_filter
   $pr_sql_filter
   $order_clause;")

echo "af_dispatched=$dispatched"
callpath run dark-factory ${1+"$@"} 2>/dev/null || true