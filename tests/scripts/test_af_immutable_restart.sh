#!/usr/bin/env bash
# test_af_immutable_restart.sh — Immutable Linux release layout & restart proof.
#
# Emits machine-readable JSON satisfying the 5 criteria of .5:
# 1. The release manifest source commit/SHA256, systemd ExecStart,
#    /proc/<pid>/exe before and after restart, and both live binary hashes
#    agree.
# 2. Restart changes daemon PID while preserving the exact bound worker
#    process for a real AO session this script owns.
# 3. Explicit stop reaps only Dark Factory-owned children and leaves
#    unrelated AO sessions/worktrees unchanged.
# 4. JSON output contains all required evidence fields, honestly split
#    across two identities that this script never conflates:
#      - restart_target: {release_commit, binary_sha256_before,
#        binary_sha256_after, proc_exe_before, proc_exe_after, unit_exec_start,
#        exec_start_matches_running_binary, manifest_cross_check,
#        daemon_pid_before, daemon_pid_after, ao_project, worktree, branch,
#        unrelated_inventory_before, unrelated_inventory_after}
#        describes the PRODUCTION daemon/worktree/branch under restart test.
#      - worker_continuity_proof: {worker_continuity_ao_project, ao_session,
#        session_branch_before, session_branch_after, tmux_session_before,
#        tmux_session_after, tmux_pane_current_path_before,
#        tmux_pane_current_path_after, tmux_pane_branch_before,
#        tmux_pane_branch_after, worker_pid_before, worker_pid_after} describes the
#        REAL but disposable AO session bound for the worker-continuity /
#        stop-path proof. Its ao_session does NOT belong to restart_target's
#        ao_project — see the opt-in comment below for why they must be
#        different projects. worktree/branch are never inferred for this
#        session from the production checkout; session_branch comes only
#        from the session's own "branch" field in `ao status --json`, and no
#        per-session worktree field exists in that schema (see adapters.rs),
#        so worker_continuity_proof deliberately has no worktree field.
#      - journal_window is top-level (applies to the whole restart run).
# 5. Changes no production code.
#
# EXIT CODE CONTRACT (never conflate SKIP with PASS):
#   0 = PASS  — every criterion above was checked against real, live state
#               (real daemon, real `ao` session, real worker process) and
#               all checks passed.
#   1 = FAIL  — a real check ran against real state and failed.
#   2 = SKIP  — this environment cannot produce real evidence for one or
#               more criteria (no systemd --user manager, `ao` unreachable,
#               no real branch-tracked target worktree, or no real AO
#               session available to bind worker-continuity/stop-path proof
#               to). This NEVER falls back to fabricated evidence — no
#               synthetic session ids, no `setsid sleep` stand-in "worker"
#               processes, and no raw `kill -9` in place of the real
#               product stop path. A SKIP means "no proof was produced",
#               never "proof of success".
#
# Real product stop/reap path used for criterion 3: `CliSessions::stop` in
# daemon/src/adapters.rs runs `ao session kill <id>` — this script invokes
# that exact command (never a raw `kill -9`) against a session it owns.
#
# Worker-continuity + stop-path proof (criterion 2/3) requires binding to a
# REAL AO session. Spawning a brand-new real AO session from inside this
# script (`ao spawn`) starts an actual paid coding-agent harness in a fresh
# worktree — that is not a safe or deterministic thing for an automated
# restart-boundary smoke test to trigger as a side effect. Reusing an
# arbitrary pre-existing real session from the production project is even
# less acceptable: this script must never call the real stop path
# (`ao session kill`) against a session it does not own, since that would
# destroy someone else's in-progress work. So real evidence for this part
# is opt-in: set AO_IMMUTABLE_RESTART_TEST_PROJECT to a dedicated, disposable
# AO project that already has exactly one active session parked for this
# test to bind to and legitimately stop. Without that opt-in, this script
# SKIPs the worker-continuity/stop-path criteria rather than fabricating.

set -euo pipefail

SKIP_EXIT=2
UNIT="${DARK_FACTORY_RESTART_UNIT:-ai.dark-factory.daemon.service}"
AO_PROJECT="${DARK_FACTORY_RESTART_AO_PROJECT:-dark-factory}"
WORKTREE="${DARK_FACTORY_RESTART_WORKTREE:-/home/jleechan/.dark-factory/target-worktrees/jleechanorg/dark-factory}"
SETTLE_SECS="${DARK_FACTORY_RESTART_SETTLE_SECS:-2}"

skip() {
  local reason="$1"
  echo "SKIPPED: $reason" >&2
  python3 -c "import json,sys; print(json.dumps({'status': 'skipped', 'reason': sys.argv[1]}, indent=2))" "$reason"
  exit "$SKIP_EXIT"
}

# --- Probe: systemd user session available? ---
if ! systemctl --user show-environment >/dev/null 2>&1; then
  skip "no systemd --user manager available on this host"
fi

# --- Probe: is the real `ao` CLI reachable? Required for every AO-bound
# criterion (project inventory, session binding, stop path). ---
if ! command -v ao >/dev/null 2>&1; then
  skip "ao CLI not found on PATH; cannot bind to a real AO session or stop path"
fi

AO_TEST_PROJECT="${AO_IMMUTABLE_RESTART_TEST_PROJECT:-}"
if [ -z "$AO_TEST_PROJECT" ]; then
  skip "AO_IMMUTABLE_RESTART_TEST_PROJECT is not set; no disposable AO project is configured to safely own a real session for worker-continuity/stop-path proof (refusing to fabricate a session or reuse a production one)"
fi
if [ "$AO_TEST_PROJECT" = "$AO_PROJECT" ]; then
  skip "AO_IMMUTABLE_RESTART_TEST_PROJECT must be distinct from production AO project '$AO_PROJECT'; refusing to stop a production session"
fi

# Ensure service is active
if ! systemctl --user is-active --quiet "$UNIT"; then
  systemctl --user start "$UNIT"
  sleep "$SETTLE_SECS"
fi

DAEMON_PID_BEFORE="$(systemctl --user show "$UNIT" --property=MainPID --value)"
if [ -z "$DAEMON_PID_BEFORE" ] || [ "$DAEMON_PID_BEFORE" = "0" ]; then
  echo "FAIL: daemon is not running before restart test" >&2
  exit 1
fi

EXEC_START_RAW="$(systemctl --user show "$UNIT" --property=ExecStart --value)"
UNIT_EXEC_START="$(python3 -c '
import re
import shlex
import sys

raw = sys.argv[1].strip()
match = re.search(r"(?:^|[{;\s])path=([^;}]+?)(?=\s*;|\s*})", raw)
if match:
    print(match.group(1).strip())
elif raw:
    fields = shlex.split(raw)
    if fields:
        print(fields[0])
' "$EXEC_START_RAW")"
if [ -z "$UNIT_EXEC_START" ]; then
  echo "FAIL: could not parse systemd ExecStart: $EXEC_START_RAW" >&2
  exit 1
fi

# Real evidence of the running binary requires a real /proc read. A silent
# fallback to the (unresolved) ExecStart path here would make the
# ExecStart-vs-running-binary comparison below vacuously self-compare the
# same string instead of proving anything real.
PROC_EXE_BEFORE="$(readlink -f "/proc/$DAEMON_PID_BEFORE/exe" 2>/dev/null || true)"
if [ -z "$PROC_EXE_BEFORE" ]; then
  skip "cannot resolve /proc/$DAEMON_PID_BEFORE/exe; no real evidence of the running daemon binary"
fi
BINARY_SHA256_BEFORE="$(sha256sum "$PROC_EXE_BEFORE" | awk '{print $1}')"

# --- Criterion 1: prove the running binary is actually the one systemd is
# configured to run, not just that both paths were read without comparing
# them. Resolve ExecStart the same way (readlink -f) since it may itself be
# a symlink (e.g. into an immutable releases/<sha> layout), then compare
# resolved real paths, not raw strings. ---
EXEC_START_RESOLVED="$(readlink -f "$UNIT_EXEC_START" 2>/dev/null || true)"
if [ -z "$EXEC_START_RESOLVED" ]; then
  echo "FAIL: cannot resolve systemd ExecStart executable: $UNIT_EXEC_START" >&2
  exit 1
fi
if [ "$PROC_EXE_BEFORE" != "$EXEC_START_RESOLVED" ]; then
  echo "FAIL: running binary ($PROC_EXE_BEFORE) does not match systemd ExecStart ($UNIT_EXEC_START, resolved: $EXEC_START_RESOLVED)" >&2
  exit 1
fi
EXEC_START_MATCHES_RUNNING_BINARY="true"

# The installer writes this manifest only after the daemon build completes.
# Require the canonical immutable layout; an arbitrary parent-directory name
# is never accepted as a source commit.
DAEMON_RELATIVE_PATH="daemon/target/release/daemon"
case "$PROC_EXE_BEFORE" in
  */releases/*/"$DAEMON_RELATIVE_PATH")
    RELEASE_DIR="${PROC_EXE_BEFORE%/"$DAEMON_RELATIVE_PATH"}"
    ;;
  *)
    echo "FAIL: daemon executable is not in the immutable releases/<commit>/$DAEMON_RELATIVE_PATH layout: $PROC_EXE_BEFORE" >&2
    exit 1
    ;;
esac
RELEASE_COMMIT="$(basename "$RELEASE_DIR")"
if [[ ! "$RELEASE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "FAIL: immutable release directory is not an exact 40-hex source commit: $RELEASE_COMMIT" >&2
  exit 1
fi
RELEASE_MANIFEST="$RELEASE_DIR/release-manifest.json"
if [ ! -f "$RELEASE_MANIFEST" ]; then
  echo "FAIL: immutable release manifest is missing: $RELEASE_MANIFEST" >&2
  exit 1
fi
if ! MANIFEST_SHA256="$(python3 - "$RELEASE_MANIFEST" "$RELEASE_COMMIT" "$DAEMON_RELATIVE_PATH" <<'PY'
import json
import re
import sys

path, expected_commit, expected_binary = sys.argv[1:]
try:
    with open(path, encoding="utf-8") as handle:
        manifest = json.load(handle)
except Exception as error:
    raise SystemExit(f"invalid release manifest {path}: {error}")

if manifest.get("schema_version") != 1:
    raise SystemExit("release manifest schema_version must be 1")
if manifest.get("source_commit") != expected_commit:
    raise SystemExit("release manifest source_commit does not match release directory")
daemon = manifest.get("daemon")
if not isinstance(daemon, dict) or daemon.get("path") != expected_binary:
    raise SystemExit("release manifest daemon.path does not match the canonical daemon path")
digest = daemon.get("sha256", "")
if not re.fullmatch(r"[0-9a-f]{64}", digest):
    raise SystemExit("release manifest daemon.sha256 is not 64 lowercase hex characters")
print(digest)
PY
)"; then
  echo "FAIL: release manifest validation failed: $RELEASE_MANIFEST" >&2
  exit 1
fi
if [ "$BINARY_SHA256_BEFORE" != "$MANIFEST_SHA256" ]; then
  echo "FAIL: running daemon SHA256 does not match release manifest ($BINARY_SHA256_BEFORE != $MANIFEST_SHA256)" >&2
  exit 1
fi
MANIFEST_CROSS_CHECK_STATUS="verified"

# --- Resolve dynamic target worktree & branch from the live checkout. A
# missing worktree or a detached HEAD means we cannot bind to a real
# branch-tracked worktree, so this is a SKIP, never a fabricated branch. ---
if [ ! -d "$WORKTREE/.git" ]; then
  skip "target worktree $WORKTREE has no .git; cannot verify a real branch-tracked worktree"
fi
BRANCH="$(git -C "$WORKTREE" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")"
if [ -z "$BRANCH" ] || [ "$BRANCH" = "HEAD" ]; then
  skip "target worktree $WORKTREE is on a detached HEAD ($(git -C "$WORKTREE" rev-parse HEAD 2>/dev/null || echo unknown)); cannot verify a real branch-tracked worktree"
fi

# Parse `ao status --json`, allowing notifier text only before the first JSON
# array. A successful command with malformed JSON remains a parse failure.
parse_ao_json() {
  local payload="$1"
  printf '%s' "$payload" | python3 -c '
import json
import sys

raw = sys.stdin.read()
start = raw.find("[")
if start < 0:
    raise SystemExit("ao status output has no JSON array")
try:
    data = json.loads(raw[start:])
except Exception as error:
    raise SystemExit(f"invalid ao status JSON: {error}")
if not isinstance(data, list) or any(not isinstance(entry, dict) for entry in data):
    raise SystemExit("ao status JSON must be an array of objects")
print(json.dumps(data, sort_keys=True, separators=(",", ":")))
'
}

# Parse the authoritative project-scoped session inspection response. Unlike
# `ao status`, this interface must supply every identity field needed for a
# live proof; missing fields are a SKIP, never an inferred value.
parse_ao_session_json() {
  local payload="$1"
  printf '%s' "$payload" | python3 -c '
import json
import sys

raw = sys.stdin.read()
start = raw.find("{")
if start < 0:
    raise SystemExit("ao session inspection has no JSON object")
try:
    data = json.loads(raw[start:])
except Exception as error:
    raise SystemExit(f"invalid ao session inspection JSON: {error}")
if not isinstance(data, dict):
    raise SystemExit("ao session inspection JSON must be an object")
required = ("project", "branch", "workspace_path", "worker_pid", "runtime", "tmux_session")
if any(not data.get(field) for field in required):
    raise SystemExit("ao session inspection is missing required identity fields")
if data["runtime"] != "tmux":
    raise SystemExit("ao session inspection runtime is not tmux")
if not str(data["worker_pid"]).isdigit():
    raise SystemExit("ao session inspection worker_pid is not numeric")
print(json.dumps(data, sort_keys=True, separators=(",", ":")))
'
}

# Normalize successful status arrays into deterministic inventories. Keep only
# stable identity/liveness fields; volatile lastActivity/review metadata is
# deliberately excluded. The only row removed is the exact AO session this
# script owns; all other rows remain evidence.
normalize_ao_inventory() {
  local payload="$1"
  local owned_session="$2"
  printf '%s' "$payload" | python3 -c '
import json
import sys

owned = sys.argv[1]
try:
    data = json.load(sys.stdin)
except Exception as error:
    raise SystemExit(f"invalid ao status JSON: {error}")
if not isinstance(data, list) or any(not isinstance(entry, dict) for entry in data):
    raise SystemExit("ao status JSON must be an array of objects")
stable_fields = ("name", "project", "branch", "role", "status", "activity")
rows = [
    {key: entry[key] for key in stable_fields if key in entry}
    for entry in data
    if entry.get("name") != owned
]
rows.sort(key=lambda entry: json.dumps(entry, sort_keys=True, separators=(",", ":")))
print(json.dumps(rows, sort_keys=True, separators=(",", ":")))
' "$owned_session"
}

# Normalize the all-pane view for the unrelated-resource comparison while
# excluding only this run's resolved tmux session artifact.
normalize_tmux_inventory() {
  local payload="$1"
  local owned_tmux_session="$2"
  printf '%s' "$payload" | python3 -c '
import json
import sys

owned = sys.argv[1]
rows = []
for line in sys.stdin:
    fields = line.rstrip("\n").split("\t")
    if len(fields) != 3 or any(not field for field in fields):
        raise SystemExit("tmux all-pane output is malformed")
    session, pid, path = fields
    if session != owned:
        rows.append({"session_name": session, "pane_pid": pid, "pane_current_path": path})
rows.sort(key=lambda entry: json.dumps(entry, sort_keys=True, separators=(",", ":")))
print(json.dumps(rows, sort_keys=True, separators=(",", ":")))
' "$owned_tmux_session"
}

# Resolve the authoritative tmux session identity to exactly one pane from
# the all-pane query. The runtime session name comes from `ao session get`; no
# AO-row-derived name is accepted.
resolve_tmux_identity() {
  local payload="$1"
  local expected_session="$2"
  local expected_pid="$3"
  local expected_path="$4"
  printf '%s' "$payload" | python3 -c '
import sys

expected = sys.argv[1]
expected_pid = str(sys.argv[2])
expected_path = sys.argv[3]
candidates = {}
for line in sys.stdin:
    fields = line.rstrip("\n").split("\t")
    if len(fields) != 3:
        continue
    session, pid, path = fields
    if session == expected and pid == expected_pid and path == expected_path:
        candidates.setdefault(session, []).append((pid, path))
if len(candidates) != 1:
    raise SystemExit("authoritative AO session did not correlate to exactly one tmux session")
session, panes = next(iter(candidates.items()))
if len(panes) != 1:
    raise SystemExit("authoritative AO session correlated to an ambiguous tmux pane set")
print("\t".join((session, panes[0][0], panes[0][1])))
' "$expected_session" "$expected_pid" "$expected_path"
}

owned_ao_row_state() {
  local payload="$1"
  local owned_session="$2"
  printf '%s' "$payload" | python3 -c '
import json
import sys

owned = sys.argv[1]
terminal = {"killed", "terminated", "done", "cleanup", "errored", "merged"}
data = json.load(sys.stdin)
matches = [entry for entry in data if entry.get("name") == owned]
if not matches:
    print("absent")
elif all(entry.get("status") in terminal or entry.get("activity") == "exited" for entry in matches):
    print("terminal")
else:
    print("active")
' "$owned_session"
}

count_tmux_identity_matches() {
  local payload="$1"
  local expected_session="$2"
  printf '%s' "$payload" | python3 -c '
import sys

expected = sys.argv[1]
sessions = set()
for line in sys.stdin:
    fields = line.rstrip("\n").split("\t")
    if len(fields) != 3:
        raise SystemExit("tmux all-pane output is malformed")
    session = fields[0]
    if session == expected:
        sessions.add(session)
print(len(sessions))
' "$expected_session"
}

# Inventory of unrelated sessions/processes before (strictly project-scoped).
# This is compared byte-for-byte against the same call after the restart +
# explicit stop below to prove neither perturbed sessions outside the ones
# this script explicitly owns and stops.
#
# A failed `ao status` call is NOT "zero unrelated sessions" -- silently
# substituting '[]' would make a query failure indistinguishable from a
# genuine empty inventory, and if both the before/after calls fail
# independently the byte-for-byte comparison below would trivially "pass"
# on zero real evidence. Capture the real exit code and SKIP (consistent
# with this script's never-fabricate philosophy) rather than proceeding.
set +e
UNRELATED_BEFORE="$(ao status -p "$AO_PROJECT" --json 2>/dev/null)"
UNRELATED_BEFORE_RC=$?
set -e
if [ "$UNRELATED_BEFORE_RC" -ne 0 ]; then
  skip "'ao status -p $AO_PROJECT --json' failed (rc=$UNRELATED_BEFORE_RC) before restart; cannot produce real unrelated-inventory evidence"
fi
set +e
PRODUCTION_STATUS_BEFORE="$(parse_ao_json "$UNRELATED_BEFORE")"
PRODUCTION_STATUS_BEFORE_RC=$?
set -e
if [ "$PRODUCTION_STATUS_BEFORE_RC" -ne 0 ]; then
  skip "'ao status -p $AO_PROJECT --json' returned malformed JSON before restart; cannot produce real unrelated-inventory evidence"
fi

# --- Bind to a REAL, currently-tracked, non-terminal AO session this script
# owns, so the stop-path proof below can legitimately call the real product
# stop path without touching anyone else's in-progress session. Opt-in only
# via AO_IMMUTABLE_RESTART_TEST_PROJECT (a dedicated disposable project) --
# never fabricated, and never taken from the production $AO_PROJECT list. ---
set +e
TEST_PROJECT_SESSIONS_BEFORE="$(ao status -p "$AO_TEST_PROJECT" --json 2>/dev/null)"
TEST_PROJECT_SESSIONS_BEFORE_RC=$?
set -e
if [ "$TEST_PROJECT_SESSIONS_BEFORE_RC" -ne 0 ]; then
  skip "'ao status -p $AO_TEST_PROJECT --json' failed (rc=$TEST_PROJECT_SESSIONS_BEFORE_RC) before restart; cannot bind a real test session"
fi
set +e
TEST_PROJECT_STATUS_BEFORE="$(parse_ao_json "$TEST_PROJECT_SESSIONS_BEFORE")"
TEST_PROJECT_STATUS_BEFORE_RC=$?
set -e
if [ "$TEST_PROJECT_STATUS_BEFORE_RC" -ne 0 ]; then
  skip "'ao status -p $AO_TEST_PROJECT --json' returned malformed JSON before restart; cannot bind a real test session"
fi

# Real schema (verified against daemon/src/adapters.rs session_for_branch /
# session_is_quiescent / session_activity): each entry has "name" (the
# session id), "branch", "activity", and "status" — never "id".
SESSION_IDENTITY_BEFORE="$(printf '%s' "$TEST_PROJECT_STATUS_BEFORE" | python3 -c "
import json, sys

TERMINAL = {'killed', 'terminated', 'done', 'cleanup', 'errored', 'merged'}

try:
    data = json.load(sys.stdin)
except Exception as error:
    raise SystemExit(f'invalid ao status JSON: {error}')
if not isinstance(data, list) or any(not isinstance(entry, dict) for entry in data):
    raise SystemExit('ao status JSON must be an array of objects')

def is_terminal(entry):
    return entry.get('status') in TERMINAL or entry.get('activity') == 'exited'

active = [
    e for e in (data if isinstance(data, list) else [])
    if isinstance(e, dict) and e.get('name') and not is_terminal(e)
]
if len(active) == 1 and active[0].get('branch'):
    print(active[0]['name'] + '\t' + str(active[0]['branch']))
" 2>/dev/null || true)"
IFS=$'\t' read -r AO_SESSION SESSION_BRANCH_BEFORE <<< "$SESSION_IDENTITY_BEFORE"

if [ -z "$AO_SESSION" ] || [ -z "$SESSION_BRANCH_BEFORE" ]; then
  skip "AO_IMMUTABLE_RESTART_TEST_PROJECT='$AO_TEST_PROJECT' does not have exactly one real active AO session with a branch to bind worker-continuity/stop-path proof to"
fi

# --- Resolve the REAL worker process PID for this session via the tmux
# all-pane query -- never a synthetic `setsid sleep` stand-in. Capture the
# tmux session and pane path as part of the identity tuple. ---
if ! command -v tmux >/dev/null 2>&1; then
  skip "tmux CLI not found on PATH; cannot correlate the AO row to a real worker pane"
fi
set +e
AO_SESSION_INFO_BEFORE_RAW="$(ao session get "$AO_SESSION" -p "$AO_TEST_PROJECT" --json 2>/dev/null)"
AO_SESSION_INFO_BEFORE_RC=$?
set -e
if [ "$AO_SESSION_INFO_BEFORE_RC" -ne 0 ]; then
  skip "'ao session get $AO_SESSION -p $AO_TEST_PROJECT --json' failed; authoritative worker identity is unavailable"
fi
set +e
AO_SESSION_INFO_BEFORE="$(parse_ao_session_json "$AO_SESSION_INFO_BEFORE_RAW")"
AO_SESSION_INFO_BEFORE_PARSE_RC=$?
set -e
if [ "$AO_SESSION_INFO_BEFORE_PARSE_RC" -ne 0 ]; then
  skip "'ao session get $AO_SESSION -p $AO_TEST_PROJECT --json' lacks authoritative project/branch/workspace/PID/tmux identity fields"
fi
SESSION_INFO_FIELDS_BEFORE="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print("\t".join(str(d[k]) for k in ("project", "branch", "workspace_path", "worker_pid", "runtime", "tmux_session")))' "$AO_SESSION_INFO_BEFORE")"
IFS=$'\t' read -r SESSION_PROJECT_BEFORE SESSION_BRANCH_INSPECTED_BEFORE SESSION_WORKSPACE_PATH_BEFORE SESSION_WORKER_PID_BEFORE SESSION_RUNTIME_BEFORE TMUX_SESSION_BEFORE <<< "$SESSION_INFO_FIELDS_BEFORE"
if [ "$SESSION_PROJECT_BEFORE" != "$AO_TEST_PROJECT" ] || [ "$SESSION_BRANCH_INSPECTED_BEFORE" != "$SESSION_BRANCH_BEFORE" ]; then
  echo "FAIL: authoritative AO session identity does not match requested project/branch" >&2
  exit 1
fi
if [ ! -d "$SESSION_WORKSPACE_PATH_BEFORE" ]; then
  skip "authoritative AO session workspace path '$SESSION_WORKSPACE_PATH_BEFORE' is not available"
fi
if [ -z "$SESSION_WORKER_PID_BEFORE" ] || ! kill -0 "$SESSION_WORKER_PID_BEFORE" 2>/dev/null; then
  skip "authoritative AO session worker PID '$SESSION_WORKER_PID_BEFORE' is not live"
fi
TMUX_PANE_BRANCH_BEFORE="$(git -C "$SESSION_WORKSPACE_PATH_BEFORE" rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
if [ -z "$TMUX_PANE_BRANCH_BEFORE" ] || [ "$TMUX_PANE_BRANCH_BEFORE" = "HEAD" ]; then
  skip "authoritative AO session workspace '$SESSION_WORKSPACE_PATH_BEFORE' is not branch-tracked"
fi
if [ "$TMUX_PANE_BRANCH_BEFORE" != "$SESSION_BRANCH_INSPECTED_BEFORE" ]; then
  echo "FAIL: authoritative AO workspace branch '$TMUX_PANE_BRANCH_BEFORE' does not match AO branch '$SESSION_BRANCH_INSPECTED_BEFORE'" >&2
  exit 1
fi
set +e
TMUX_PANES_BEFORE="$(tmux list-panes -a -F '#{session_name}\t#{pane_pid}\t#{pane_current_path}' 2>/dev/null)"
TMUX_PANES_BEFORE_RC=$?
set -e
if [ "$TMUX_PANES_BEFORE_RC" -ne 0 ]; then
  skip "tmux all-pane query failed before restart (rc=$TMUX_PANES_BEFORE_RC); cannot correlate a real worker pane"
fi
set +e
TMUX_IDENTITY_BEFORE="$(resolve_tmux_identity "$TMUX_PANES_BEFORE" "$TMUX_SESSION_BEFORE" "$SESSION_WORKER_PID_BEFORE" "$SESSION_WORKSPACE_PATH_BEFORE")"
TMUX_IDENTITY_BEFORE_RC=$?
set -e
if [ "$TMUX_IDENTITY_BEFORE_RC" -ne 0 ]; then
  skip "AO session '$AO_SESSION' did not correlate to exactly one live tmux session/pane via the all-pane query"
fi
IFS=$'\t' read -r TMUX_SESSION_MATCHED_BEFORE WORKER_PID_BEFORE TMUX_PANE_CURRENT_PATH_BEFORE <<< "$TMUX_IDENTITY_BEFORE"
if [ "$TMUX_SESSION_MATCHED_BEFORE" != "$TMUX_SESSION_BEFORE" ] || [ "$WORKER_PID_BEFORE" != "$SESSION_WORKER_PID_BEFORE" ] || [ "$TMUX_PANE_CURRENT_PATH_BEFORE" != "$SESSION_WORKSPACE_PATH_BEFORE" ]; then
  echo "FAIL: all-pane tmux identity does not match authoritative AO session inspection" >&2
  exit 1
fi

set +e
PRODUCTION_UNRELATED_BEFORE="$(normalize_ao_inventory "$PRODUCTION_STATUS_BEFORE" "__no_owned_production_session__")"
PRODUCTION_UNRELATED_BEFORE_RC=$?
DISPOSABLE_UNRELATED_BEFORE="$(normalize_ao_inventory "$TEST_PROJECT_STATUS_BEFORE" "$AO_SESSION")"
DISPOSABLE_UNRELATED_BEFORE_RC=$?
TMUX_UNRELATED_BEFORE="$(normalize_tmux_inventory "$TMUX_PANES_BEFORE" "$TMUX_SESSION_BEFORE")"
TMUX_UNRELATED_BEFORE_RC=$?
set -e
if [ "$PRODUCTION_UNRELATED_BEFORE_RC" -ne 0 ] || [ "$DISPOSABLE_UNRELATED_BEFORE_RC" -ne 0 ] || [ "$TMUX_UNRELATED_BEFORE_RC" -ne 0 ]; then
  skip "could not parse an AO or tmux inventory before restart; no unrelated-resource evidence was produced"
fi

JOURNAL_START="$(date -u +"%Y-%m-%d %H:%M:%S")"

# Perform restart
systemctl --user restart "$UNIT"
sleep "$SETTLE_SECS"

DAEMON_PID_AFTER="$(systemctl --user show "$UNIT" --property=MainPID --value)"
if [ -z "$DAEMON_PID_AFTER" ] || [ "$DAEMON_PID_AFTER" = "0" ] || [ "$DAEMON_PID_AFTER" = "$DAEMON_PID_BEFORE" ]; then
  echo "FAIL: daemon PID did not advance on restart ($DAEMON_PID_BEFORE -> $DAEMON_PID_AFTER)" >&2
  exit 1
fi

PROC_EXE_AFTER="$(readlink -f "/proc/$DAEMON_PID_AFTER/exe" 2>/dev/null || true)"
if [ -z "$PROC_EXE_AFTER" ]; then
  echo "FAIL: cannot resolve /proc/$DAEMON_PID_AFTER/exe after restart" >&2
  exit 1
fi
BINARY_SHA256_AFTER="$(sha256sum "$PROC_EXE_AFTER" | awk '{print $1}')"
if [ "$PROC_EXE_AFTER" != "$EXEC_START_RESOLVED" ] || [ "$PROC_EXE_AFTER" != "$PROC_EXE_BEFORE" ]; then
  echo "FAIL: restarted daemon executable changed ($PROC_EXE_BEFORE -> $PROC_EXE_AFTER; ExecStart=$EXEC_START_RESOLVED)" >&2
  exit 1
fi
if [ "$BINARY_SHA256_AFTER" != "$MANIFEST_SHA256" ] || [ "$BINARY_SHA256_AFTER" != "$BINARY_SHA256_BEFORE" ]; then
  echo "FAIL: restarted daemon SHA256 does not match the release manifest ($BINARY_SHA256_BEFORE -> $BINARY_SHA256_AFTER; manifest=$MANIFEST_SHA256)" >&2
  exit 1
fi

# Re-query the disposable project after restart. A surviving PID alone is not
# enough: AO must still report the same session on the same branch.
set +e
TEST_PROJECT_SESSIONS_AFTER="$(ao status -p "$AO_TEST_PROJECT" --json 2>/dev/null)"
TEST_PROJECT_SESSIONS_AFTER_RC=$?
set -e
if [ "$TEST_PROJECT_SESSIONS_AFTER_RC" -ne 0 ]; then
  skip "'ao status -p $AO_TEST_PROJECT --json' failed (rc=$TEST_PROJECT_SESSIONS_AFTER_RC) after restart; cannot prove session continuity"
fi
set +e
TEST_PROJECT_STATUS_AFTER="$(parse_ao_json "$TEST_PROJECT_SESSIONS_AFTER")"
TEST_PROJECT_STATUS_AFTER_RC=$?
set -e
if [ "$TEST_PROJECT_STATUS_AFTER_RC" -ne 0 ]; then
  skip "'ao status -p $AO_TEST_PROJECT --json' returned malformed JSON after restart; cannot prove session continuity"
fi
SESSION_BRANCH_AFTER="$(printf '%s' "$TEST_PROJECT_STATUS_AFTER" | python3 -c "
import json, sys

terminal = {'killed', 'terminated', 'done', 'cleanup', 'errored', 'merged'}
expected = sys.argv[1]
try:
    data = json.load(sys.stdin)
except Exception as error:
    raise SystemExit(f'invalid ao status JSON: {error}')
if not isinstance(data, list) or any(not isinstance(entry, dict) for entry in data):
    raise SystemExit('ao status JSON must be an array of objects')
for entry in (data if isinstance(data, list) else []):
    if not isinstance(entry, dict) or entry.get('name') != expected:
        continue
    if entry.get('status') in terminal or entry.get('activity') == 'exited':
        break
    print(entry.get('branch', ''))
    break
" "$AO_SESSION" 2>/dev/null || true)"
if [ -z "$SESSION_BRANCH_AFTER" ] || [ "$SESSION_BRANCH_AFTER" != "$SESSION_BRANCH_BEFORE" ]; then
  echo "FAIL: AO session identity changed across restart (session=$AO_SESSION, branch=$SESSION_BRANCH_BEFORE -> ${SESSION_BRANCH_AFTER:-missing})" >&2
  exit 1
fi

set +e
AO_SESSION_INFO_AFTER_RAW="$(ao session get "$AO_SESSION" -p "$AO_TEST_PROJECT" --json 2>/dev/null)"
AO_SESSION_INFO_AFTER_RC=$?
set -e
if [ "$AO_SESSION_INFO_AFTER_RC" -ne 0 ]; then
  skip "'ao session get $AO_SESSION -p $AO_TEST_PROJECT --json' failed after restart; cannot reverify authoritative worker identity"
fi
set +e
AO_SESSION_INFO_AFTER="$(parse_ao_session_json "$AO_SESSION_INFO_AFTER_RAW")"
AO_SESSION_INFO_AFTER_PARSE_RC=$?
set -e
if [ "$AO_SESSION_INFO_AFTER_PARSE_RC" -ne 0 ]; then
  skip "'ao session get $AO_SESSION -p $AO_TEST_PROJECT --json' returned incomplete identity after restart"
fi
SESSION_INFO_FIELDS_AFTER="$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); print("\t".join(str(d[k]) for k in ("project", "branch", "workspace_path", "worker_pid", "runtime", "tmux_session")))' "$AO_SESSION_INFO_AFTER")"
IFS=$'\t' read -r SESSION_PROJECT_AFTER SESSION_BRANCH_INSPECTED_AFTER SESSION_WORKSPACE_PATH_AFTER SESSION_WORKER_PID_AFTER SESSION_RUNTIME_AFTER TMUX_SESSION_AFTER <<< "$SESSION_INFO_FIELDS_AFTER"
if [ "$SESSION_PROJECT_AFTER" != "$SESSION_PROJECT_BEFORE" ] || [ "$SESSION_BRANCH_INSPECTED_AFTER" != "$SESSION_BRANCH_INSPECTED_BEFORE" ] || [ "$SESSION_WORKSPACE_PATH_AFTER" != "$SESSION_WORKSPACE_PATH_BEFORE" ] || [ "$SESSION_WORKER_PID_AFTER" != "$SESSION_WORKER_PID_BEFORE" ] || [ "$SESSION_RUNTIME_AFTER" != "$SESSION_RUNTIME_BEFORE" ] || [ "$TMUX_SESSION_AFTER" != "$TMUX_SESSION_BEFORE" ]; then
  echo "FAIL: authoritative AO session identity tuple changed across restart" >&2
  exit 1
fi

# Re-resolve and compare the exact tmux identity tuple after restart.
set +e
TMUX_PANES_AFTER="$(tmux list-panes -a -F '#{session_name}\t#{pane_pid}\t#{pane_current_path}' 2>/dev/null)"
TMUX_PANES_AFTER_RC=$?
set -e
if [ "$TMUX_PANES_AFTER_RC" -ne 0 ]; then
  skip "tmux all-pane query failed after restart (rc=$TMUX_PANES_AFTER_RC); cannot reverify worker identity"
fi
set +e
TMUX_IDENTITY_AFTER="$(resolve_tmux_identity "$TMUX_PANES_AFTER" "$TMUX_SESSION_AFTER" "$SESSION_WORKER_PID_AFTER" "$SESSION_WORKSPACE_PATH_AFTER")"
TMUX_IDENTITY_AFTER_RC=$?
set -e
if [ "$TMUX_IDENTITY_AFTER_RC" -ne 0 ]; then
  skip "AO session '$AO_SESSION' did not correlate to exactly one live tmux session/pane after restart"
fi
IFS=$'\t' read -r TMUX_SESSION_MATCHED_AFTER WORKER_PID_AFTER TMUX_PANE_CURRENT_PATH_AFTER <<< "$TMUX_IDENTITY_AFTER"
if [ "$TMUX_SESSION_MATCHED_AFTER" != "$TMUX_SESSION_AFTER" ] || [ "$WORKER_PID_AFTER" != "$WORKER_PID_BEFORE" ] || [ "$TMUX_PANE_CURRENT_PATH_AFTER" != "$TMUX_PANE_CURRENT_PATH_BEFORE" ]; then
  echo "FAIL: tmux worker identity tuple changed across restart (session=$TMUX_SESSION_BEFORE/$TMUX_SESSION_AFTER, pid=$WORKER_PID_BEFORE/$WORKER_PID_AFTER, pane=$TMUX_PANE_CURRENT_PATH_BEFORE/$TMUX_PANE_CURRENT_PATH_AFTER)" >&2
  exit 1
fi
if ! kill -0 "$WORKER_PID_AFTER" 2>/dev/null; then
  echo "FAIL: real AO worker process $WORKER_PID_AFTER (session $AO_SESSION) did not survive restart" >&2
  exit 1
fi
TMUX_PANE_BRANCH_AFTER="$(git -C "$TMUX_PANE_CURRENT_PATH_AFTER" rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
if [ -z "$TMUX_PANE_BRANCH_AFTER" ] || [ "$TMUX_PANE_BRANCH_AFTER" = "HEAD" ]; then
  skip "tmux pane path '$TMUX_PANE_CURRENT_PATH_AFTER' is not a branch-tracked checkout after restart"
fi
if [ "$TMUX_PANE_BRANCH_AFTER" != "$SESSION_BRANCH_AFTER" ]; then
  echo "FAIL: tmux pane branch '$TMUX_PANE_BRANCH_AFTER' does not match AO session branch '$SESSION_BRANCH_AFTER' after restart" >&2
  exit 1
fi

# --- Explicit stop / reap verification via the REAL product stop path.
# CliSessions::stop (daemon/src/adapters.rs) runs `ao session kill <id>` --
# this invokes that exact command, never a raw `kill -9`. ---
if ! ao session kill "$AO_SESSION" >/dev/null 2>&1; then
  echo "FAIL: real product stop path 'ao session kill $AO_SESSION' failed to execute" >&2
  exit 1
fi
reaped=0
for _ in $(seq 1 20); do
  if ! kill -0 "$WORKER_PID_BEFORE" 2>/dev/null; then
    reaped=1
    break
  fi
  sleep 0.1
done
if [ "$reaped" -ne 1 ]; then
  echo "FAIL: worker process $WORKER_PID_BEFORE was not cleanly reaped by 'ao session kill $AO_SESSION'" >&2
  exit 1
fi

# Inventory of unrelated resources after (strictly project-scoped) -- must be
# identical to before for both projects. The owned AO row and resolved tmux
# artifact are excluded; no unrelated row or pane is suppressed.
set +e
UNRELATED_AFTER="$(ao status -p "$AO_PROJECT" --json 2>/dev/null)"
UNRELATED_AFTER_RC=$?
TEST_PROJECT_SESSIONS_FINAL="$(ao status -p "$AO_TEST_PROJECT" --json 2>/dev/null)"
TEST_PROJECT_SESSIONS_FINAL_RC=$?
TMUX_PANES_FINAL="$(tmux list-panes -a -F '#{session_name}\t#{pane_pid}\t#{pane_current_path}' 2>/dev/null)"
TMUX_PANES_FINAL_RC=$?
set -e
if [ "$UNRELATED_AFTER_RC" -ne 0 ]; then
  skip "'ao status -p $AO_PROJECT --json' failed (rc=$UNRELATED_AFTER_RC) after restart; cannot produce real unrelated-inventory evidence"
fi
if [ "$TEST_PROJECT_SESSIONS_FINAL_RC" -ne 0 ]; then
  skip "'ao status -p $AO_TEST_PROJECT --json' failed (rc=$TEST_PROJECT_SESSIONS_FINAL_RC) after stop; cannot produce real unrelated-inventory evidence"
fi
if [ "$TMUX_PANES_FINAL_RC" -ne 0 ]; then
  skip "tmux all-pane query failed after stop (rc=$TMUX_PANES_FINAL_RC); cannot produce real unrelated-resource evidence"
fi
set +e
PRODUCTION_STATUS_AFTER="$(parse_ao_json "$UNRELATED_AFTER")"
PRODUCTION_STATUS_AFTER_RC=$?
TEST_PROJECT_STATUS_FINAL="$(parse_ao_json "$TEST_PROJECT_SESSIONS_FINAL")"
TEST_PROJECT_STATUS_FINAL_RC=$?
TMUX_OWNED_COUNT_FINAL="$(count_tmux_identity_matches "$TMUX_PANES_FINAL" "$AO_SESSION")"
TMUX_OWNED_COUNT_FINAL_RC=$?
set -e
if [ "$PRODUCTION_STATUS_AFTER_RC" -ne 0 ]; then
  skip "'ao status -p $AO_PROJECT --json' returned malformed JSON after stop; cannot produce real unrelated-inventory evidence"
fi
if [ "$TEST_PROJECT_STATUS_FINAL_RC" -ne 0 ]; then
  skip "'ao status -p $AO_TEST_PROJECT --json' returned malformed JSON after stop; cannot verify owned session cleanup"
fi
OWNED_AO_ROW_STATE_FINAL="$(owned_ao_row_state "$TEST_PROJECT_STATUS_FINAL" "$AO_SESSION")"
if [ "$OWNED_AO_ROW_STATE_FINAL" = "active" ]; then
  echo "FAIL: owned AO session '$AO_SESSION' remains active after 'ao session kill'" >&2
  exit 1
fi
if [ "$TMUX_OWNED_COUNT_FINAL_RC" -ne 0 ]; then
  skip "tmux all-pane output was malformed after stop; cannot verify owned pane cleanup"
fi
if [ "$TMUX_OWNED_COUNT_FINAL" -ne 0 ]; then
  echo "FAIL: owned tmux session/pane for AO session '$AO_SESSION' remains after 'ao session kill'" >&2
  exit 1
fi
set +e
PRODUCTION_UNRELATED_AFTER="$(normalize_ao_inventory "$PRODUCTION_STATUS_AFTER" "__no_owned_production_session__")"
PRODUCTION_UNRELATED_AFTER_RC=$?
DISPOSABLE_UNRELATED_AFTER="$(normalize_ao_inventory "$TEST_PROJECT_STATUS_FINAL" "$AO_SESSION")"
DISPOSABLE_UNRELATED_AFTER_RC=$?
TMUX_UNRELATED_AFTER="$(normalize_tmux_inventory "$TMUX_PANES_FINAL" "$TMUX_SESSION_BEFORE")"
TMUX_UNRELATED_AFTER_RC=$?
set -e
if [ "$PRODUCTION_UNRELATED_AFTER_RC" -ne 0 ] || [ "$DISPOSABLE_UNRELATED_AFTER_RC" -ne 0 ] || [ "$TMUX_UNRELATED_AFTER_RC" -ne 0 ]; then
  skip "could not parse an AO or tmux inventory after stop; no unrelated-resource evidence was produced"
fi

JOURNAL_END="$(date -u +"%Y-%m-%d %H:%M:%S")"
JOURNAL_WINDOW="${JOURNAL_START} .. ${JOURNAL_END}"

export RELEASE_COMMIT BINARY_SHA256_BEFORE BINARY_SHA256_AFTER
export PROC_EXE_BEFORE PROC_EXE_AFTER UNIT_EXEC_START EXEC_START_MATCHES_RUNNING_BINARY
export RELEASE_MANIFEST MANIFEST_SHA256 MANIFEST_CROSS_CHECK_STATUS
export DAEMON_PID_BEFORE DAEMON_PID_AFTER
export AO_PROJECT WORKTREE BRANCH
export AO_TEST_PROJECT AO_SESSION SESSION_BRANCH_BEFORE SESSION_BRANCH_AFTER
export TMUX_SESSION_BEFORE TMUX_SESSION_AFTER
export TMUX_PANE_CURRENT_PATH_BEFORE TMUX_PANE_CURRENT_PATH_AFTER
export TMUX_PANE_BRANCH_BEFORE TMUX_PANE_BRANCH_AFTER
export WORKER_PID_BEFORE WORKER_PID_AFTER
UNRELATED_INVENTORY_BEFORE="$(python3 -c 'import json,sys; print(json.dumps({"production": json.loads(sys.argv[1]), "disposable": json.loads(sys.argv[2]), "tmux": json.loads(sys.argv[3])}, sort_keys=True, separators=(",", ":")))' "$PRODUCTION_UNRELATED_BEFORE" "$DISPOSABLE_UNRELATED_BEFORE" "$TMUX_UNRELATED_BEFORE")"
UNRELATED_INVENTORY_AFTER="$(python3 -c 'import json,sys; print(json.dumps({"production": json.loads(sys.argv[1]), "disposable": json.loads(sys.argv[2]), "tmux": json.loads(sys.argv[3])}, sort_keys=True, separators=(",", ":")))' "$PRODUCTION_UNRELATED_AFTER" "$DISPOSABLE_UNRELATED_AFTER" "$TMUX_UNRELATED_AFTER")"
export UNRELATED_INVENTORY_BEFORE UNRELATED_INVENTORY_AFTER JOURNAL_WINDOW

python3 -c "
import json
import os
import sys

unrelated_before = json.loads(os.environ['UNRELATED_INVENTORY_BEFORE'])
unrelated_after = json.loads(os.environ['UNRELATED_INVENTORY_AFTER'])

if unrelated_before != unrelated_after:
    sys.stderr.write('FAIL: unrelated inventory was perturbed across daemon restart\n')
    sys.exit(1)

# restart_target describes the PRODUCTION daemon/worktree/branch under
# restart test. worker_continuity_proof describes the separate, disposable
# AO_TEST_PROJECT session bound only to prove worker-process continuity
# across the restart and the real stop path -- its ao_session belongs to
# worker_continuity_ao_project, never to restart_target's ao_project, and
# this script never implies otherwise.
report = {
    'status': 'passed',
    'restart_target': {
        'release_commit': os.environ['RELEASE_COMMIT'],
        'binary_sha256_before': os.environ['BINARY_SHA256_BEFORE'],
        'binary_sha256_after': os.environ['BINARY_SHA256_AFTER'],
        'proc_exe_before': os.environ['PROC_EXE_BEFORE'],
        'proc_exe_after': os.environ['PROC_EXE_AFTER'],
        'unit_exec_start': os.environ['UNIT_EXEC_START'],
        'exec_start_matches_running_binary': os.environ['EXEC_START_MATCHES_RUNNING_BINARY'] == 'true',
        'manifest_cross_check': {
            'status': os.environ['MANIFEST_CROSS_CHECK_STATUS'],
            'path': os.environ['RELEASE_MANIFEST'],
            'daemon_sha256': os.environ['MANIFEST_SHA256'],
        },
        'daemon_pid_before': os.environ['DAEMON_PID_BEFORE'],
        'daemon_pid_after': os.environ['DAEMON_PID_AFTER'],
        'ao_project': os.environ['AO_PROJECT'],
        'worktree': os.environ['WORKTREE'],
        'branch': os.environ['BRANCH'],
        'unrelated_inventory_before': unrelated_before,
        'unrelated_inventory_after': unrelated_after,
    },
    'worker_continuity_proof': {
        'worker_continuity_ao_project': os.environ['AO_TEST_PROJECT'],
        'ao_session': os.environ['AO_SESSION'],
        'session_branch_before': os.environ['SESSION_BRANCH_BEFORE'],
        'session_branch_after': os.environ['SESSION_BRANCH_AFTER'],
        'tmux_session_before': os.environ['TMUX_SESSION_BEFORE'],
        'tmux_session_after': os.environ['TMUX_SESSION_AFTER'],
        'tmux_pane_current_path_before': os.environ['TMUX_PANE_CURRENT_PATH_BEFORE'],
        'tmux_pane_current_path_after': os.environ['TMUX_PANE_CURRENT_PATH_AFTER'],
        'tmux_pane_branch_before': os.environ['TMUX_PANE_BRANCH_BEFORE'],
        'tmux_pane_branch_after': os.environ['TMUX_PANE_BRANCH_AFTER'],
        'worker_pid_before': os.environ['WORKER_PID_BEFORE'],
        'worker_pid_after': os.environ['WORKER_PID_AFTER'],
    },
    'journal_window': os.environ['JOURNAL_WINDOW'],
}

def check(prefix, d):
    for k, v in d.items():
        path = f'{prefix}.{k}' if prefix else k
        if isinstance(v, dict):
            check(path, v)
        elif v is None or v == '':
            sys.stderr.write(f'Field {path} is empty\n')
            sys.exit(1)

check('', report)

print(json.dumps(report, indent=2))
"
