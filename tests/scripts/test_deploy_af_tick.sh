#!/usr/bin/env bash
# test_deploy_af_tick.sh — verifies daemon/scripts/deploy-af-tick.sh, the
# audited deploy step for the ai.dark-factory.af-tick daemon's execution
# checkout (bead jleechan-vxs8).
#
# Covers: refuses missing target dir, refuses non-main branch, refuses dirty
# tree, no-op is idempotent and logs noop:true, a real advance logs the
# old_sha -> new_sha transition to the JSONL audit log, and --dry-run never
# mutates the target checkout.
#
# Run with: bash tests/scripts/test_deploy_af_tick.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DEPLOY="$ROOT/daemon/scripts/deploy-af-tick.sh"

PASS=0
FAIL=0
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

[ -x "$DEPLOY" ] && { echo "PASS: deploy-af-tick.sh is executable"; PASS=$((PASS+1)); } \
    || { echo "FAIL: deploy-af-tick.sh is not executable"; FAIL=$((FAIL+1)); }

SCRATCH_DIR="$(mktemp -d -t test-deploy-af-tick.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

ORIGIN="$SCRATCH_DIR/origin.git"
git init -q --bare --initial-branch=main "$ORIGIN"

# Seed origin with a deterministic initial main commit so clones never see
# an empty repository (git-version-dependent warning/error on bare-origin
# clone). This keeps the fixture deterministic across CI and local runs.
SEED="$SCRATCH_DIR/seed"
git init -q --initial-branch=main "$SEED"
(
    cd "$SEED"
    git config user.email test@test.com
    git config user.name test
    echo a > f.txt
    git add f.txt
    git commit -q -m init
    git remote add origin "$ORIGIN"
    git push -q origin main
)
# The bare origin's HEAD symref is set at `git init --bare` time from the
# host's init.defaultBranch (defaulting to "master" when unset). It is NOT
# automatically repointed by a later push of a "main" branch, so any
# subsequent `git clone` of $ORIGIN lands on a dangling/wrong-named unborn
# branch instead of "main" -- explicitly repoint it so later clones (e.g.
# $SECOND below) always land on main regardless of host git config.
git --git-dir="$ORIGIN" symbolic-ref HEAD refs/heads/main

rm -rf "$SEED"

WORK="$SCRATCH_DIR/work"
git clone -q "$ORIGIN" "$WORK"
DEPLOY_LOG="$SCRATCH_DIR/deploy.jsonl"

# ---------------------------------------------------------------------------
# Case A: missing / non-git target dir -> rc=3
# ---------------------------------------------------------------------------
set +e
bash "$DEPLOY" --target-dir "$SCRATCH_DIR/does-not-exist" >/dev/null 2>&1
rc=$?
set -e
assert "refuses missing target dir (rc=3)" "3" "$rc"

# ---------------------------------------------------------------------------
# Case B: non-main branch -> rc=5, no fetch/merge attempted
# ---------------------------------------------------------------------------
(cd "$WORK" && git checkout -q -b factory/some-feature)
set +e
err="$(bash "$DEPLOY" --target-dir "$WORK" 2>&1)"
rc=$?
set -e
assert "refuses non-main branch (rc=5)" "5" "$rc"
case "$err" in
    *"not main"*) echo "PASS: non-main error message mentions 'not main'"; PASS=$((PASS+1)) ;;
    *) echo "FAIL: non-main error message unexpected: $err"; FAIL=$((FAIL+1)) ;;
esac
(cd "$WORK" && git checkout -q main)

# ---------------------------------------------------------------------------
# Case C: dirty tree -> rc=4
# ---------------------------------------------------------------------------
(cd "$WORK" && echo dirty >> f.txt)
set +e
err="$(bash "$DEPLOY" --target-dir "$WORK" 2>&1)"
rc=$?
set -e
assert "refuses dirty tree (rc=4)" "4" "$rc"
case "$err" in
    *"uncommitted changes"*) echo "PASS: dirty-tree error message mentions uncommitted changes"; PASS=$((PASS+1)) ;;
    *) echo "FAIL: dirty-tree error message unexpected: $err"; FAIL=$((FAIL+1)) ;;
esac
(cd "$WORK" && git checkout -q -- f.txt)

# ---------------------------------------------------------------------------
# Case D: already up to date -> rc=0, JSONL logs noop:true, old_sha==new_sha
# ---------------------------------------------------------------------------
set +e
out="$(AFD_DEPLOY_LOG="$DEPLOY_LOG" bash "$DEPLOY" --target-dir "$WORK" 2>&1)"
rc=$?
set -e
assert "no-op deploy succeeds (rc=0)" "0" "$rc"
case "$out" in
    *"no-op"*) echo "PASS: no-op message printed"; PASS=$((PASS+1)) ;;
    *) echo "FAIL: no-op message missing: $out"; FAIL=$((FAIL+1)) ;;
esac
last_log_line="$(tail -1 "$DEPLOY_LOG")"
noop_val="$(printf '%s' "$last_log_line" | python3 -c 'import json,sys; print(json.load(sys.stdin)["noop"])')"
assert "no-op JSONL entry has noop=True" "True" "$noop_val"
old_sha="$(printf '%s' "$last_log_line" | python3 -c 'import json,sys; print(json.load(sys.stdin)["old_sha"])')"
new_sha="$(printf '%s' "$last_log_line" | python3 -c 'import json,sys; print(json.load(sys.stdin)["new_sha"])')"
assert "no-op JSONL entry old_sha == new_sha" "$old_sha" "$new_sha"

# ---------------------------------------------------------------------------
# Case E: real advance -> rc=0, logs distinct old_sha -> new_sha, HEAD moves
# ---------------------------------------------------------------------------
SECOND="$SCRATCH_DIR/second"
git clone -q "$ORIGIN" "$SECOND"
(
    cd "$SECOND"
    git config user.email test@test.com
    git config user.name test
    echo b >> f.txt
    git add f.txt
    git commit -q -m second
    # Push via HEAD:main (not a bare "main" refspec) so this works even if
    # the clone's local branch name ever diverges from "main" for any reason.
    git push -q origin HEAD:main
)
before_sha="$(cd "$WORK" && git rev-parse HEAD)"
set +e
out="$(AFD_DEPLOY_LOG="$DEPLOY_LOG" bash "$DEPLOY" --target-dir "$WORK" 2>&1)"
rc=$?
set -e
assert "real advance deploy succeeds (rc=0)" "0" "$rc"
after_sha="$(cd "$WORK" && git rev-parse HEAD)"
if [ "$before_sha" != "$after_sha" ]; then
    echo "PASS: target checkout HEAD advanced ($before_sha -> $after_sha)"
    PASS=$((PASS + 1))
else
    echo "FAIL: target checkout HEAD did not advance"
    FAIL=$((FAIL + 1))
fi
case "$out" in
    *"DEPLOYED"*"->"*) echo "PASS: deploy output logs SHA transition"; PASS=$((PASS+1)) ;;
    *) echo "FAIL: deploy output missing SHA transition: $out"; FAIL=$((FAIL+1)) ;;
esac
last_log_line="$(tail -1 "$DEPLOY_LOG")"
noop_val="$(printf '%s' "$last_log_line" | python3 -c 'import json,sys; print(json.load(sys.stdin)["noop"])')"
assert "real advance JSONL entry has noop=False" "False" "$noop_val"
logged_new_sha="$(printf '%s' "$last_log_line" | python3 -c 'import json,sys; print(json.load(sys.stdin)["new_sha"])')"
assert "real advance JSONL new_sha matches post-deploy HEAD" "$after_sha" "$logged_new_sha"

# ---------------------------------------------------------------------------
# Case F: --dry-run never mutates the target checkout
# ---------------------------------------------------------------------------
(
    cd "$SECOND"
    echo c >> f.txt
    git add f.txt
    git commit -q -m third
    git push -q origin HEAD:main
)
before_sha="$(cd "$WORK" && git rev-parse HEAD)"
bash "$DEPLOY" --target-dir "$WORK" --dry-run >/dev/null 2>&1
after_sha="$(cd "$WORK" && git rev-parse HEAD)"
assert "--dry-run does not mutate target checkout" "$before_sha" "$after_sha"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
