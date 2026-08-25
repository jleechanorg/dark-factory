#!/usr/bin/env bash
# Lane D end-to-end test for /web-advice fail-open pipeline.
# Refs: docs/web-advice-failopen-design.md, docs/web-advice-failopen-e2e-log.md
#
# Mission: drive `dark-factory` against a real test PR so the new
# `type="web_advice"` handler is exercised against an actual diff. The
# fail-open invariant — every invocation returns outcome=success to the
# .dot engine regardless of the panel verdict — is what we are proving.
#
# Operator pre-flight: do NOT invoke against the real pr-655 follow-up
# beads (mergeable:null / cross-repo dispatch / etc.). This script targets
# the lane D test PR only.

set -euo pipefail

# ---------------------------------------------------------------------------
# Environment
# ---------------------------------------------------------------------------

# Resolve repo root so the script works regardless of cwd.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LANE_D_REPO="${LANE_D_REPO:-$(cd "${SCRIPT_DIR}/.." && pwd)}"

# Lane B/C artifacts live in the lane D worktree, not the primary checkout.
# The dark-factory binary uses DARK_FACTORY_HOME to find the runner venv
# + runner module + .dot pipeline files.
export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-${LANE_D_REPO}}"
export DARK_FACTORY_HOLDOUTS="${DARK_FACTORY_HOLDOUTS:-$HOME/projects/dark-factory-holdouts}"
export PATH="$HOME/.local/bin:$PATH"

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

TEST_PR_REPO="jleechanorg/dark-factory"
TEST_PR_BRANCH="test/web-advice-failopen-pr"
TEST_PR_NUMBER="664"
TEST_PR_URL="https://github.com/${TEST_PR_REPO}/pull/${TEST_PR_NUMBER}"
TEST_PR_WORKTREE="/home/jleechan/.worktrees/dark-factory/test-web-advice-failopen"

FEATURE_NAME="web-advice-failopen-test-pr"
TARGET_REPO="${TEST_PR_REPO}"

CXDB_PATH="/tmp/web-advice-failopen-test-cxdb.sqlite"
DIFF_PATH="/tmp/pr_${TEST_PR_NUMBER}_full.patch"
HEAD_SHA="f3caec5ca33b10d5b759266487f479d187188159"

# ---------------------------------------------------------------------------
# Pre-flight guards — fail closed if anything is missing.
# ---------------------------------------------------------------------------

if [[ ! -d "${DARK_FACTORY_HOME}" ]]; then
  echo "ERROR: DARK_FACTORY_HOME does not exist: ${DARK_FACTORY_HOME}" >&2
  exit 1
fi
if [[ ! -x "${DARK_FACTORY_HOME}/.venv/bin/python" ]]; then
  echo "ERROR: dark-factory venv missing at ${DARK_FACTORY_HOME}/.venv" >&2
  echo "Run: (cd ${DARK_FACTORY_HOME} && ./install.sh)" >&2
  exit 1
fi
if [[ ! -d "${DARK_FACTORY_HOLDOUTS}" ]]; then
  echo "ERROR: DARK_FACTORY_HOLDOUTS does not exist: ${DARK_FACTORY_HOLDOUTS}" >&2
  exit 1
fi

# Test worktree must exist with the lane D branch checked out.
# (git worktrees use `.git` as a file pointer, NOT a directory — check exists.)
if [[ ! -e "${TEST_PR_WORKTREE}/.git" ]]; then
  echo "ERROR: test PR worktree not found at ${TEST_PR_WORKTREE}" >&2
  echo "Create it first with: cd ~/projects/dark-factory && git worktree add ${TEST_PR_WORKTREE} -b ${TEST_PR_BRANCH} origin/main" >&2
  exit 1
fi

# Diff patch must exist (regenerated from the test PR head if missing).
if [[ ! -f "${DIFF_PATH}" ]]; then
  echo "Regenerating diff patch from ${TEST_PR_WORKTREE}..."
  (cd "${TEST_PR_WORKTREE}" && git format-patch -1 HEAD~1..HEAD --stdout > "${DIFF_PATH}")
fi

# Fresh CXDB so we measure only this run.
rm -f "${CXDB_PATH}" "${CXDB_PATH}.tmp"

# ---------------------------------------------------------------------------
# Build the runner invocation
# ---------------------------------------------------------------------------

# `--backend echo` is the documented smoke-test mode — codergen echoes
# its prompt and returns success without burning tokens. The strict gates
# (gate_skeptic / parallel_reviewer / gate_es / gate_er / gate_cs) use
# their own backend_priority chain (codex,minimax,agy), independent of
# --backend, so they will still attempt to call out. Claude remains an
# explicit opt-in only and is not part of the default chain.
# Failures in strict gates route to the fix loop; only after gate_cs
# succeeds does web_advice run.
INVOCATION=(
  dark-factory
  --pipeline pipelines/factory/web-advice-failopen.dot
  --goal "Lane D E2E: validate fail-open /web-advice routing on PR #${TEST_PR_NUMBER} test branch"
  --workdir "${TEST_PR_WORKTREE}"
  --backend echo
  --feature "${FEATURE_NAME}"
  --require-holdouts
  --state "pr_url=${TEST_PR_URL}"
  --state "head_sha=${HEAD_SHA}"
  --state "diff_path=${DIFF_PATH}"
  --state "target_repo=${TARGET_REPO}"
  --state "evidence_dir=docs/web-advice-failopen-e2e-log"
  --cxdb "${CXDB_PATH}"
  --no-perf-log
)

# ---------------------------------------------------------------------------
# Print pre-run summary
# ---------------------------------------------------------------------------

cat <<PRE_RUN
================================================================================
Lane D — /web-advice fail-open end-to-end test
================================================================================
  Test PR       : ${TEST_PR_URL}   (DRAFT — do NOT merge)
  Test branch   : ${TEST_PR_BRANCH} @ ${HEAD_SHA}
  Diff patch    : ${DIFF_PATH}
  CXDB          : ${CXDB_PATH}        (fresh — rm -f before run)
  Pipeline      : pipelines/factory/web-advice-failopen.dot
  Working dir   : ${TEST_PR_WORKTREE}
  DARK_FACTORY_HOME   : ${DARK_FACTORY_HOME}
  DARK_FACTORY_HOLDOUTS: ${DARK_FACTORY_HOLDOUTS}
  Backend       : echo (codergen smoke mode; strict gates use own priority chain)
  Pre-seeded state:
    pr_url       = ${TEST_PR_URL}
    head_sha     = ${HEAD_SHA}
    diff_path    = ${DIFF_PATH}
    feature      = ${FEATURE_NAME}
    target_repo  = ${TARGET_REPO}
    evidence_dir = docs/web-advice-failopen-e2e-log

About to execute (verbatim):
  $(printf '%q ' "${INVOCATION[@]}")
================================================================================
PRE_RUN

# ---------------------------------------------------------------------------
# Execute the run and capture output
# ---------------------------------------------------------------------------

RUN_LOG="/tmp/web-advice-failopen-test-run.log"
RUN_RC=0

echo "=== dark-factory run started at $(date -u +%Y-%m-%dT%H:%M:%SZ) ===" | tee "${RUN_LOG}"

# Use stdbuf to keep the live tail unbuffered for visibility.
stdbuf -oL -eL "${INVOCATION[@]}" 2>&1 | tee -a "${RUN_LOG}" || RUN_RC=$?

echo "=== dark-factory run ended at $(date -u +%Y-%m-%dT%H:%M:%SZ) (rc=${RUN_RC}) ===" | tee -a "${RUN_LOG}"

# ---------------------------------------------------------------------------
# Parse the JSON summary out of the log so we can build a structured report.
# ---------------------------------------------------------------------------

SUMMARY_JSON=""
if grep -q '"steps":' "${RUN_LOG}" 2>/dev/null; then
  # The CLI prints a JSON summary as the last stdout blob (between newlines).
  SUMMARY_JSON="$(awk '
    BEGIN { capture=0; depth=0 }
    /^\{/ { capture=1; buf="" }
    capture { buf = buf $0 "\n" }
    /^\}/ { print buf; capture=0; exit }
  ' "${RUN_LOG}")"
fi

FINAL_OUTCOME="(unknown)"
TRACE_LINES=()
EVENT_COUNT=0

if [[ -n "${SUMMARY_JSON}" ]] && command -v python3 >/dev/null 2>&1; then
  read -r FINAL_OUTCOME EVENT_COUNT TRACE_LINES < <(
    python3 -c '
import json, sys
try:
    summary = json.load(sys.stdin)
except Exception as e:
    print(f"(parse-error:{e})", 0, "")
    sys.exit(0)
final = summary.get("final_outcome", "(none)")
events = summary.get("steps", 0)
trace = summary.get("trace", [])
lines = []
for r in trace:
    name = r.get("node", "?")
    out = r.get("outcome", "?")
    preview = (r.get("preview") or "").replace("\n", " ")[:80]
    reviewer = r.get("reviewer_backend") or ""
    fb = r.get("fallback_used") or ""
    timed = r.get("timed_out") or False
    extras = []
    if reviewer: extras.append(f"backend={reviewer}")
    if fb: extras.append(f"fallback={fb}")
    if timed: extras.append("timed_out=true")
    extra = (" " + " ".join(extras)) if extras else ""
    lines.append(f"  - {name:30s} {out:10s} {preview}{extra}")
print(final, events, "\n".join(lines))
' <<< "${SUMMARY_JSON}"
  )
fi

CXDB_EVENTS=0
if [[ -f "${CXDB_PATH}" ]]; then
  CXDB_EVENTS=$(sqlite3 "${CXDB_PATH}" "SELECT COUNT(*) FROM steps;" 2>/dev/null || echo 0)
fi

WEB_ADVICE_OUTCOME="(not reached)"
WEB_ADVICE_PANEL_LIVE="(unknown)"
WEB_ADVICE_DECISION="(unknown)"
WEB_ADVICE_PR_COMMENT_URL=""
if [[ -f "${CXDB_PATH}" ]]; then
  # Try to find a web_advice_review row in the CXDB steps table.
  # The metadata is JSON-stored in a column; sqlite3's json1 may or may not be loaded.
  WEB_ADVICE_OUTCOME=$(sqlite3 "${CXDB_PATH}" \
    "SELECT IFNULL(json_extract(metadata, '$.event_type'),'(none)') FROM steps WHERE node='web_advice' LIMIT 1;" \
    2>/dev/null || echo "(cxdb-json1-missing)")
  WEB_ADVICE_PANEL_LIVE=$(sqlite3 "${CXDB_PATH}" \
    "SELECT IFNULL(json_extract(metadata, '$.panel_seats_live'),'(none)') FROM steps WHERE node='web_advice' LIMIT 1;" \
    2>/dev/null || echo "(none)")
  WEB_ADVICE_DECISION=$(sqlite3 "${CXDB_PATH}" \
    "SELECT IFNULL(json_extract(metadata, '$.decision'),'(none)') FROM steps WHERE node='web_advice' LIMIT 1;" \
    2>/dev/null || echo "(none)")
  WEB_ADVICE_PR_COMMENT_URL=$(sqlite3 "${CXDB_PATH}" \
    "SELECT IFNULL(json_extract(metadata, '$.pr_comment_url'),'(none)') FROM steps WHERE node='web_advice' LIMIT 1;" \
    2>/dev/null || echo "(none)")
fi

PR_COMMENT_COUNT=$(gh pr view "${TEST_PR_NUMBER}" --json comments --jq '.comments | length' 2>/dev/null || echo "(gh-error)")

# ---------------------------------------------------------------------------
# Post-run summary
# ---------------------------------------------------------------------------

cat <<POST_RUN

================================================================================
POST-RUN SUMMARY
================================================================================
  Final pipeline outcome : ${FINAL_OUTCOME}
  History steps          : ${EVENT_COUNT}  (summary trace)
  CXDB events            : ${CXDB_EVENTS}  (raw steps in ${CXDB_PATH})
  PR comment count       : ${PR_COMMENT_COUNT}
  Failed (rc)            : ${RUN_RC}

  Web-advice node outcomes:
    event_type            : ${WEB_ADVICE_OUTCOME}
    panel_seats_live      : ${WEB_ADVICE_PANEL_LIVE}
    decision              : ${WEB_ADVICE_DECISION}
    pr_comment_url        : ${WEB_ADVICE_PR_COMMENT_URL}

  Per-node trace from engine history:
${TRACE_LINES:-  (no trace parsed — see ${RUN_LOG})}

Fail-open invariant:
  web_advice ALWAYS returns outcome=success at the .dot layer.
  → Whether the panel reached or not, the runner will not show web_advice
    as a failure cause. If web_advice.outcome above is "(not reached)",
    that is a routing outcome (strict gates did not pass), NOT a
    fail-open regression.

Artifacts:
  - Run log        : ${RUN_LOG}
  - CXDB SQLite    : ${CXDB_PATH}
  - Diff patch     : ${DIFF_PATH}
  - Design doc     : ${DARK_FACTORY_HOME}/docs/web-advice-failopen-design.md
  - Pipeline .dot  : ${DARK_FACTORY_HOME}/pipelines/factory/web-advice-failopen.dot
  - Handler        : ${DARK_FACTORY_HOME}/runner/handler_web_advice.py
  - E2E log        : ${DARK_FACTORY_HOME}/docs/web-advice-failopen-e2e-log.md

DO NOT MERGE PR #${TEST_PR_NUMBER}. The draft PR remains for operator review.
================================================================================
POST_RUN

# Always exit with the original run rc so callers see what happened.
exit "${RUN_RC}"
