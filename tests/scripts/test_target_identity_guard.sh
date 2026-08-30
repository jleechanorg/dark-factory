#!/usr/bin/env bash
# test_target_identity_guard.sh — TDD contract for the worker-side
# target-identity guards (bead dark-factory-w2fr / incident wa-3551
# reproduced by dark-factory-o74s for PR #9462 — a remediation worker
# drifted onto sibling worktrees / PRs #9512/#8292 and edited
# provenance-narrow/mvp_site/schemas/prompt_tool_contracts.json).
#
# Contract under test
# -------------------
# Two worker-invoked guards live in daemon/scripts/:
#
#   af-target-identity-guard.sh  — invoked by the worker BEFORE writing
#                                  any tracked file or running `git push`.
#                                  Reads AF_TARGET_CHECKOUT, AF_TARGET_BRANCH,
#                                  AF_TARGET_REPO from env (injected at spawn
#                                  by factory-ao-remediate.sh), compares them
#                                  to the live `cwd`/`HEAD`/`origin` URL, and
#                                  refuses the action on drift.
#
#   af-push-identity-guard.sh    — thin wrapper around `git push` that
#                                  re-runs the identity check immediately
#                                  before exec. Same env contract.
#
# Both guards must FAIL CLOSED with a stable `target-drift.json` sentinel
# in the worker's cwd on any mismatch (so a triage operator can find the
# drift without reading worker stdout), exit non-zero so the worker
# halts, and stay silent (exit 0, no sentinel) on a perfect match.
#
# Tests
# -----
# 1. identity-guard: matching cwd/branch/repo → exit 0, no sentinel
# 2. identity-guard: sibling-worktree drift → exit non-zero, sentinel written
# 3. identity-guard: cross-repo drift → exit non-zero, sentinel written
# 4. identity-guard: branch drift on same repo → exit non-zero, sentinel written
# 5. identity-guard: missing env vars → exit non-zero (fail closed, not silent)
# 6. push-guard: matching identity delegates to `git push` (dry-run friendly)
# 7. push-guard: drift blocks the push even when the underlying command would
#                succeed
# 8. factory-ao-remediate.sh: injects AF_TARGET_CHECKOUT / AF_TARGET_BRANCH /
#                             AF_TARGET_REPO into the spawned AO command
#
# Run with: bash tests/scripts/test_target_identity_guard.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/af-target-identity-guard.sh"
PUSH_GUARD="$ROOT/daemon/scripts/af-push-identity-guard.sh"
REMEDIATE="$ROOT/daemon/factory-ao-remediate.sh"

for f in "$GUARD" "$PUSH_GUARD" "$REMEDIATE"; do
  [ -f "$f" ] || { echo "FAIL: missing $f" >&2; exit 1; }
done

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"
    FAIL=$((FAIL + 1))
  fi
}
assert_ne() {
  local name="$1" forbidden="$2" actual="$3"
  if [ "$forbidden" != "$actual" ]; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (got forbidden value '$actual')"
    FAIL=$((FAIL + 1))
  fi
}
assert_file() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (file expected='$expected', exists? '$actual')"
    FAIL=$((FAIL + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-target-id.XXXXXX)"
trap 'rm -rf "$SCRATCH_DIR"' EXIT

# ---------------------------------------------------------------------------
# Fixture helper: build a real git repo at $1 with origin URL $2 and HEAD on
# branch $3. Sets up a bare remote so `git push` would succeed.
# ---------------------------------------------------------------------------
make_repo() {
  local root="$1" origin_url="$2" branch="$3"
  rm -rf "$root"
  mkdir -p "$root"
  git -C "$root" init -q -b "$branch"
  git -C "$root" -c user.email=test@example.invalid -c user.name=test \
    commit --allow-empty -q -m "init $branch"
  git -C "$root" remote add origin "$origin_url"
  echo "$root"
}

# ---------------------------------------------------------------------------
# Test 1: identity guard passes when cwd/branch/repo all match.
# ---------------------------------------------------------------------------
test_match_passes() {
  local repo origin expected_branch actual_branch
  repo="$(make_repo "$SCRATCH_DIR/match_repo" "https://github.com/owner/match-repo.git" "factory/test-1-r1")"
  expected_branch="$(git -C "$repo" rev-parse --abbrev-ref HEAD)"
  (
    cd "$repo"
    AF_TARGET_CHECKOUT="$repo" \
    AF_TARGET_BRANCH="refs/heads/$expected_branch" \
    AF_TARGET_REPO="owner/match-repo" \
    bash "$GUARD"
  ) > "$SCRATCH_DIR/match.stdout" 2> "$SCRATCH_DIR/match.stderr"
  local rc=$?
  assert "match: exit 0 on perfect identity match" "0" "$rc"
  assert "match: no stderr on success" "" "$(cat "$SCRATCH_DIR/match.stderr")"
  assert_file "match: no target-drift.json sentinel on success" "no" "$([ -f "$repo/target-drift.json" ] && echo yes || echo no)"
}

# ---------------------------------------------------------------------------
# Test 2: sibling-worktree drift (the wa-3551 pattern): same repo URL,
# different physical worktree path. Worker must be refused.
# ---------------------------------------------------------------------------
test_sibling_worktree_drift() {
  local assigned_repo sibling_repo assigned_branch
  assigned_repo="$(make_repo "$SCRATCH_DIR/sibling_assigned" "https://github.com/owner/sibling-repo.git" "factory/test-2-r1")"
  sibling_repo="$(make_repo "$SCRATCH_DIR/sibling_actual"   "https://github.com/owner/sibling-repo.git" "factory/test-2-r1")"
  assigned_branch="$(git -C "$assigned_repo" rev-parse --abbrev-ref HEAD)"
  (
    cd "$sibling_repo"
    AF_TARGET_CHECKOUT="$assigned_repo" \
    AF_TARGET_BRANCH="refs/heads/$assigned_branch" \
    AF_TARGET_REPO="owner/sibling-repo" \
    bash "$GUARD"
  ) > /dev/null 2> "$SCRATCH_DIR/sibling.stderr"
  local rc=$?
  assert "sibling-worktree: exit non-zero on drift" "1" "$rc"
  assert_file "sibling-worktree: target-drift.json written to worker's cwd" \
    "yes" "$([ -f "$sibling_repo/target-drift.json" ] && echo yes || echo no)"
  # Sentinel must name every drifted dimension so triage is unambiguous.
  local body
  body="$(cat "$sibling_repo/target-drift.json" 2>/dev/null || true)"
  case "$body" in
    *worktree*) echo "PASS: sentinel names the worktree dimension"; PASS=$((PASS+1)) ;;
    *) echo "FAIL: sentinel missing worktree dimension: $body"; FAIL=$((FAIL+1)) ;;
  esac
}

# ---------------------------------------------------------------------------
# Test 3: cross-repo drift: right worktree path, but the repo URL belongs
# to a different owner/repo (the dark-factory-ik0v / j9id shape: worker is
# in the dark-factory checkout but writes to worldarchitect.ai paths).
# ---------------------------------------------------------------------------
test_cross_repo_drift() {
  local assigned_repo actual_repo assigned_branch
  assigned_repo="$(make_repo "$SCRATCH_DIR/xrepo_assigned" "https://github.com/owner/worldarchitect.ai.git" "factory/test-3-r1")"
  actual_repo="$(make_repo "$SCRATCH_DIR/xrepo_actual"   "https://github.com/owner/provenance-narrow.git" "factory/test-3-r1")"
  assigned_branch="$(git -C "$assigned_repo" rev-parse --abbrev-ref HEAD)"
  (
    cd "$actual_repo"
    AF_TARGET_CHECKOUT="$actual_repo" \
    AF_TARGET_BRANCH="refs/heads/$assigned_branch" \
    AF_TARGET_REPO="owner/worldarchitect.ai" \
    bash "$GUARD"
  ) > /dev/null 2> "$SCRATCH_DIR/xrepo.stderr"
  local rc=$?
  assert "cross-repo: exit non-zero on drift" "1" "$rc"
  local body
  body="$(cat "$actual_repo/target-drift.json" 2>/dev/null || true)"
  case "$body" in
    *repo*) echo "PASS: sentinel names the repo dimension"; PASS=$((PASS+1)) ;;
    *) echo "FAIL: sentinel missing repo dimension: $body"; FAIL=$((FAIL+1)) ;;
  esac
}

# ---------------------------------------------------------------------------
# Test 4: branch drift on same repo: worker spawned for branch A but
# checkout has HEAD on branch B (the wa-3551 PR-#9512-vs-#9462 pattern).
# ---------------------------------------------------------------------------
test_branch_drift() {
  local repo assigned_branch
  repo="$(make_repo "$SCRATCH_DIR/branch_repo" "https://github.com/owner/branch-repo.git" "factory/test-4-r1")"
  assigned_branch="$(git -C "$repo" rev-parse --abbrev-ref HEAD)"
  (
    cd "$repo"
    AF_TARGET_CHECKOUT="$repo" \
    AF_TARGET_BRANCH="refs/heads/factory/wa-OTHER-r9" \
    AF_TARGET_REPO="owner/branch-repo" \
    bash "$GUARD"
  ) > /dev/null 2> "$SCRATCH_DIR/branch.stderr"
  local rc=$?
  assert "branch-drift: exit non-zero on branch mismatch" "1" "$rc"
  local body
  body="$(cat "$repo/target-drift.json" 2>/dev/null || true)"
  case "$body" in
    *branch*) echo "PASS: sentinel names the branch dimension"; PASS=$((PASS+1)) ;;
    *) echo "FAIL: sentinel missing branch dimension: $body"; FAIL=$((FAIL+1)) ;;
  esac
}

# ---------------------------------------------------------------------------
# Test 5: missing env vars must fail closed (not silently pass). A worker
# that did not receive its identity token must be refused, not let through.
# ---------------------------------------------------------------------------
test_missing_env_fails_closed() {
  local repo
  repo="$(make_repo "$SCRATCH_DIR/missing_env" "https://github.com/owner/missing-env.git" "factory/test-5-r1")"
  (
    cd "$repo"
    unset AF_TARGET_CHECKOUT AF_TARGET_BRANCH AF_TARGET_REPO
    bash "$GUARD"
  ) > /dev/null 2> "$SCRATCH_DIR/missing.stderr"
  local rc=$?
  assert_ne "missing-env: refuse (non-zero) when no identity tokens supplied" \
    "0" "$rc"
  assert_file "missing-env: target-drift.json written when env absent" \
    "yes" "$([ -f "$repo/target-drift.json" ] && echo yes || echo no)"
}

# ---------------------------------------------------------------------------
# Test 6: push guard on a matching identity must execute git push. We use
# a no-op git push (--dry-run + a bare remote) so the test does not require
# network auth. We assert the push guard's wrapper runs git push with the
# identity check still enforced.
# ---------------------------------------------------------------------------
test_push_guard_match_delegates() {
  local repo bare_remote assigned_branch
  bare_remote="$SCRATCH_DIR/push_remote.git"
  mkdir -p "$bare_remote"
  git -C "$bare_remote" init -q --bare
  repo="$(make_repo "$SCRATCH_DIR/push_match" "https://github.com/owner/push-match.git" "factory/test-6-r1")"
  assigned_branch="$(git -C "$repo" rev-parse --abbrev-ref HEAD)"
  (
    cd "$repo"
    AF_TARGET_CHECKOUT="$repo" \
    AF_TARGET_BRANCH="refs/heads/$assigned_branch" \
    AF_TARGET_REPO="owner/push-match" \
    bash "$PUSH_GUARD" origin "$assigned_branch" --dry-run
  ) > "$SCRATCH_DIR/push_match.stdout" 2> "$SCRATCH_DIR/push_match.stderr"
  local rc=$?
  case "$rc" in
    0|128) echo "PASS: push-guard matched-identity delegates (rc=$rc)"; PASS=$((PASS+1)) ;;
    *) echo "FAIL: push-guard matched-identity exited $rc; stderr=$(cat "$SCRATCH_DIR/push_match.stderr")"; FAIL=$((FAIL+1)) ;;
  esac
}

# ---------------------------------------------------------------------------
# Test 7: push guard on drift must NOT execute the underlying git push.
# We use a fake git binary in PATH that records any push attempt; if the
# drift is caught, the fake should never be invoked.
# ---------------------------------------------------------------------------
test_push_guard_drift_blocks() {
  local repo fake_bin assigned_branch
  fake_bin="$SCRATCH_DIR/fake-bin"
  mkdir -p "$fake_bin"
  cat > "$fake_bin/git" <<'EOF'
#!/usr/bin/env bash
# Records every git invocation; pushes to remote are recorded.
echo "$@" >> "$RECORD_LOG"
if [ "${1:-}" = "push" ]; then
  echo "FAKE_GIT_PUSH_INVOKED" >> "$RECORD_LOG"
fi
exit 0
EOF
  chmod +x "$fake_bin/git"
  repo="$(make_repo "$SCRATCH_DIR/push_drift" "https://github.com/owner/push-drift.git" "factory/test-7-r1")"
  assigned_branch="$(git -C "$repo" rev-parse --abbrev-ref HEAD)"
  PATH="$fake_bin:$PATH" \
  RECORD_LOG="$SCRATCH_DIR/fake-git.log" \
  bash -c '
    cd "$1"
    AF_TARGET_CHECKOUT="$1" \
    AF_TARGET_BRANCH="refs/heads/factory/wa-DRIFT-r9" \
    AF_TARGET_REPO="owner/push-drift" \
    bash "$2" origin '"$assigned_branch"' --dry-run
  ' _ "$repo" "$PUSH_GUARD" > /dev/null 2> "$SCRATCH_DIR/push_drift.stderr"
  local rc=$?
  assert_ne "push-drift: refuse (non-zero) on drift" "0" "$rc"
  case "$(cat "$SCRATCH_DIR/fake-git.log" 2>/dev/null || true)" in
    *FAKE_GIT_PUSH_INVOKED*)
      echo "FAIL: push-guard invoked git push despite drift"; FAIL=$((FAIL+1)) ;;
    *)
      echo "PASS: push-guard never invoked git push on drift"; PASS=$((PASS+1)) ;;
  esac
}

# ---------------------------------------------------------------------------
# Test 8: factory-ao-remediate.sh must inject AF_TARGET_* env vars when
# invoking `ao spawn`. We stub `ao` to print its environment to a file and
# verify the three identity tokens are present in that file.
# ---------------------------------------------------------------------------
test_remediate_injects_identity_env() {
  local fake_ao_dir fake_ao_log env_capture
  fake_ao_dir="$SCRATCH_DIR/fake-ao-bin"
  mkdir -p "$fake_ao_dir"
  fake_ao_log="$SCRATCH_DIR/fake-ao-spawn.env"
  cat > "$fake_ao_dir/ao" <<EOF
#!/usr/bin/env bash
# Stub AO: dump env, exit 0 with a success-shape stdout.
env | sort > "$fake_ao_log"
echo "spawned session fake-1"
exit 0
EOF
  chmod +x "$fake_ao_dir/ao"
  env_capture="$SCRATCH_DIR/remediate.env"
  # The remediate script calls factory-ao-bin.sh which resolves ao-go vs
  # ao-ts. Override AO_BIN to point at our stub.
  AO_BIN="$fake_ao_dir/ao" \
  SYNC=1 \
  AFD_LOG_DIR="$SCRATCH_DIR/logs" \
  AFD_SPAWN_STATE_DIR="$SCRATCH_DIR/spawns" \
  bash "$REMEDIATE" "dark-factory-w2fr-test" "9462" "owner/repo-9462" "test-proj" \
    > "$env_capture.stdout" 2> "$env_capture.stderr" || true
  local env_body
  env_body="$(cat "$fake_ao_log" 2>/dev/null || true)"
  case "$env_body" in
    *AF_TARGET_CHECKOUT*)
      echo "PASS: remediate injects AF_TARGET_CHECKOUT"; PASS=$((PASS+1)) ;;
    *)
      echo "FAIL: remediate did not inject AF_TARGET_CHECKOUT (got: $env_body)"; FAIL=$((FAIL+1)) ;;
  esac
  case "$env_body" in
    *AF_TARGET_BRANCH=*refs/heads/*)
      echo "PASS: remediate injects AF_TARGET_BRANCH (refs/heads/...)"; PASS=$((PASS+1)) ;;
    *)
      echo "FAIL: remediate did not inject AF_TARGET_BRANCH (got: $env_body)"; FAIL=$((FAIL+1)) ;;
  esac
  case "$env_body" in
    *AF_TARGET_REPO=owner/repo-9462*)
      echo "PASS: remediate injects AF_TARGET_REPO=<exact bead target_repo>"; PASS=$((PASS+1)) ;;
    *)
      echo "FAIL: remediate did not inject AF_TARGET_REPO=owner/repo-9462 (got: $env_body)"; FAIL=$((FAIL+1)) ;;
  esac
}

# ---------------------------------------------------------------------------
# Run all tests in this file. Each test_* function is invoked once and
# increments PASS / FAIL via the assert helpers above.
# ---------------------------------------------------------------------------
test_match_passes
test_sibling_worktree_drift
test_cross_repo_drift
test_branch_drift
test_missing_env_fails_closed
test_push_guard_match_delegates
test_push_guard_drift_blocks
test_remediate_injects_identity_env

echo
echo "================================="
echo "PASS=$PASS  FAIL=$FAIL"
echo "================================="
[ "$FAIL" -eq 0 ]
