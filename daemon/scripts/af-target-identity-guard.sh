#!/usr/bin/env bash
# Bead dark-factory-w2fr — fail-closed worker-side target-identity guard.
#
# Live incident that motivated this guard
# --------------------------------------
# dark-factory-o74s (drive-existing-PR remediation for PR #9462) spawned
# wa-3551, which operated on PRs #9512 and #8292 and modified
# provenance-narrow/mvp_site/schemas/prompt_tool_contracts.json — paths
# in a DIFFERENT repository from the assigned target. The factory-side
# cwd guard (`tools::check_cwd_guard`, bead jleechan-jw4c) only ran at
# spawn time and only validated the worktree path; it did NOT validate
# the branch or repo, and the Bash dispatch path
# (`factory-ao-remediate.sh` -> `ao spawn --claim-pr`) had no identity
# check at all. A worker that drifted after spawn (or never had the
# right identity tokens in the first place) could write to any tracked
# file in any checkout, silently.
#
# Contract
# --------
# Invoked by the worker BEFORE writing any tracked file or running
# `git push`. Reads the four identity tokens that `factory-ao-remediate.sh`
# injected at spawn time:
#
#   AF_TARGET_CHECKOUT  absolute path of the worker's assigned worktree
#   AF_TARGET_BRANCH    fully-qualified branch (`refs/heads/<name>`) the
#                       worker should be sitting on
#   AF_TARGET_REPO      `owner/name` form of the repo the worker's
#                       `origin` should point at
#   AF_BEAD_ID          the factory bead this worker is remediating
#                       (recorded into the drift sentinel for triage)
#   AF_PR_NUMBER        the PR number this worker is remediating
#                       (recorded into the drift sentinel for triage)
#
# On any drift, the guard:
#   1. exits non-zero (so the worker halts immediately),
#   2. writes `<cwd>/target-drift.json` with every drifted dimension
#      named (worktree / branch / repo) so a triage operator can find
#      the drift without grepping worker stdout,
#   3. emits a single human-readable line to stderr naming the drift,
#   4. NEVER mutates git state — refuses to write the drift sentinel
#      if the cwd is not a git worktree (so a stray invocation cannot
#      leak sentinel files into operator checkouts).
#
# On a perfect match, the guard exits 0 silently and writes nothing.
# This is the contract the coder worker relies on; changing it without
# also updating the dispatch path's prompt preamble is a regression.
set -uo pipefail

# --- 0. Sanity: refuse if invoked outside a git worktree --------------------
if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "[af-target-guard] REFUSING — invoked outside a git worktree (cwd=$(pwd))" >&2
  exit 1
fi

# --- 1. Read identity tokens. Fail closed on missing env vars --------------
missing=()
[ -n "${AF_TARGET_CHECKOUT:-}" ] || missing+=("AF_TARGET_CHECKOUT")
[ -n "${AF_TARGET_BRANCH:-}"   ] || missing+=("AF_TARGET_BRANCH")
[ -n "${AF_TARGET_REPO:-}"     ] || missing+=("AF_TARGET_REPO")

if [ "${#missing[@]}" -gt 0 ]; then
  echo "[af-target-guard] REFUSING — missing identity tokens: ${missing[*]}" >&2
  echo "[af-target-guard] The factory dispatch path must inject AF_TARGET_{CHECKOUT,BRANCH,REPO} before the worker can write code." >&2
  cat > "$(pwd)/target-drift.json" <<EOF
{
  "bead_id": "${AF_BEAD_ID:-unknown}",
  "pr_number": "${AF_PR_NUMBER:-unknown}",
  "drift_kinds": ["missing-identity-tokens"],
  "missing_tokens": [$(printf '"%s",' "${missing[@]}" | sed 's/,$//')],
  "detected_at": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "resolution": "HUMAN_HELD — the factory dispatch path failed to inject AF_TARGET_{CHECKOUT,BRANCH,REPO}; the operator must reconcile before any further dispatch."
}
EOF
  exit 1
fi

# --- 2. Resolve actual identity from the live cwd --------------------------
actual_cwd="$(realpath "$(pwd)")"
expected_cwd="$(realpath "$AF_TARGET_CHECKOUT")"

actual_branch_raw="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")"
# A detached HEAD shows empty / `HEAD`. Treat that as drift — the worker
# must be on a real local branch to push.
if [ -z "$actual_branch_raw" ] || [ "$actual_branch_raw" = "HEAD" ]; then
  echo "[af-target-guard] REFUSING — actual HEAD is detached (no branch); worker must check out ${AF_TARGET_BRANCH}" >&2
  cat > "$(pwd)/target-drift.json" <<EOF
{
  "bead_id": "${AF_BEAD_ID:-unknown}",
  "pr_number": "${AF_PR_NUMBER:-unknown}",
  "drift_kinds": ["branch"],
  "branch": {"expected":"$AF_TARGET_BRANCH","actual":"detached HEAD"},
  "detected_at": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "resolution": "HUMAN_HELD — the operator must reconcile the worker's branch identity before any further dispatch."
}
EOF
  echo "[af-target-guard] Drift sentinel written to $(pwd)/target-drift.json — read it to triage." >&2
  exit 1
fi
actual_branch="refs/heads/$actual_branch_raw"

actual_repo_raw="$(git remote get-url origin 2>/dev/null || echo "")"
actual_repo="$(printf '%s' "$actual_repo_raw" \
  | tr '[:upper:]' '[:lower:]' \
  | sed -E 's#^(https?://|ssh://[^@]+@|git@[^:]+:)##; s#^github\.com[:/]+##; s#^([^/]+/[^/.]+)(\.git)?/?$#\1#')"
expected_repo="$(printf '%s' "$AF_TARGET_REPO" | tr '[:upper:]' '[:lower:]' | sed -E 's#\.git$##')"

# --- 3. Compare each dimension, accumulate drift ---------------------------
drift_worktree=0
drift_branch=0
drift_repo=0
drift_kinds=()
drift_details=()

if [ "$actual_cwd" != "$expected_cwd" ]; then
  drift_worktree=1
  drift_kinds+=("worktree")
  drift_details+=("\"worktree\":{\"expected\":\"$expected_cwd\",\"actual\":\"$actual_cwd\"}")
fi

if [ "$actual_branch" != "$AF_TARGET_BRANCH" ]; then
  drift_branch=1
  drift_kinds+=("branch")
  drift_details+=("\"branch\":{\"expected\":\"$AF_TARGET_BRANCH\",\"actual\":\"$actual_branch\"}")
fi

if [ "$actual_repo" != "$expected_repo" ]; then
  drift_repo=1
  drift_kinds+=("repo")
  drift_details+=("\"repo\":{\"expected\":\"$expected_repo\",\"actual\":\"$actual_repo\"}")
fi

# --- 4. If any drift, write sentinel and refuse -----------------------------
if [ "${#drift_kinds[@]}" -gt 0 ]; then
  kinds_json="$(printf '"%s",' "${drift_kinds[@]}" | sed 's/,$//')"
  details_json="$(IFS=, ; echo "${drift_details[*]}")"
  cat > "$(pwd)/target-drift.json" <<EOF
{
  "bead_id": "${AF_BEAD_ID:-unknown}",
  "pr_number": "${AF_PR_NUMBER:-unknown}",
  "drift_kinds": [${kinds_json}],
  ${details_json},
  "detected_at": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "resolution": "HUMAN_HELD — the operator must reconcile the worker's worktree / branch / repo identity before any further dispatch."
}
EOF
  echo "[af-target-guard] REFUSING — target drift detected on dimensions:" >&2
  echo "[af-target-guard]   expected cwd=$expected_cwd" >&2
  echo "[af-target-guard]   actual   cwd=$actual_cwd" >&2
  echo "[af-target-guard]   expected branch=$AF_TARGET_BRANCH" >&2
  echo "[af-target-guard]   actual   branch=$actual_branch" >&2
  echo "[af-target-guard]   expected repo=$expected_repo" >&2
  echo "[af-target-guard]   actual   repo=$actual_repo" >&2
  echo "[af-target-guard] Drift sentinel written to $(pwd)/target-drift.json — read it to triage." >&2
  exit 1
fi

exit 0
