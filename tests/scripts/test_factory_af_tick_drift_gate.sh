#!/usr/bin/env bash
# test_factory_af_tick_drift_gate.sh — verifies the Gate-0-style checkout
# drift-refusal check in daemon/factory-af-tick.sh (bead jleechan-vxs8).
#
# Incident this guards against: the launchd daemon executes whatever branch
# happens to be checked out in its execution root (normally
# ~/projects/dark-factory, a dev working tree shared with interactive
# sessions). On 2026-07-11 the tree sat on a crashing feature branch for
# hours, then silently switched to a different branch — neither state was a
# deliberate deploy. The tick script must refuse to do dispatch work when
# its own checkout is not on main, is dirty, or has drifted from
# origin/main, rather than silently running whatever code is on disk.
#
# This test builds an ISOLATED fixture git repo (a bare "origin" + a working
# clone) with a slimmed-down copy of factory-af-tick.sh's arg-parsing
# preamble plus the drift-check block, so it exercises the exact drift-check
# logic without depending on the real dark-factory checkout's branch state
# (CI checks out PR branches in detached HEAD, so asserting against the real
# checkout would be flaky / order-dependent).
#
# Run with: bash tests/scripts/test_factory_af_tick_drift_gate.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TICK="$ROOT/daemon/factory-af-tick.sh"

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

# ---------------------------------------------------------------------------
# Source-level checks: the drift gate must exist, default ON, be documented
# in the exit-code contract header, and be skippable via env var.
# ---------------------------------------------------------------------------
if grep -q 'AFD_SKIP_DRIFT_CHECK' "$TICK" && grep -q 'REFUSING TICK' "$TICK"; then
    echo "PASS: factory-af-tick.sh contains the drift-refusal gate"
    PASS=$((PASS + 1))
else
    echo "FAIL: factory-af-tick.sh missing the drift-refusal gate"
    FAIL=$((FAIL + 1))
fi

if grep -qE '\$rc=10\s' "$TICK"; then
    echo "PASS: exit-code contract header documents rc=10 (drift)"
    PASS=$((PASS + 1))
else
    echo "FAIL: exit-code contract header missing rc=10 (drift) documentation"
    FAIL=$((FAIL + 1))
fi

# ---------------------------------------------------------------------------
# Behavioral checks: run the ACTUAL drift-check block (extracted verbatim
# from factory-af-tick.sh) against an isolated fixture repo, so branch /
# dirty-tree / drift states are fully controlled rather than depending on
# whatever branch this CI job happens to have checked out.
# ---------------------------------------------------------------------------
SCRATCH_DIR="$(mktemp -d -t test-af-tick-drift.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

# Extract the drift-check block verbatim so this test can never drift out of
# sync with the real implementation (no hand-copied duplicate logic).
DRIFT_BLOCK="$SCRATCH_DIR/drift_block.sh"
awk '/^# ---------- Gate 0: refuse to tick/,/^fi$/' "$TICK" > "$DRIFT_BLOCK"
if [ ! -s "$DRIFT_BLOCK" ]; then
    echo "FAIL: could not extract Gate 0 block from $TICK (awk range match empty)"
    FAIL=$((FAIL + 1))
else
    echo "PASS: extracted Gate 0 block from $TICK ($(wc -l < "$DRIFT_BLOCK" | tr -d ' ') lines)"
    PASS=$((PASS + 1))
fi

run_drift_block() {
    # Runs the extracted block inside the given repo dir; ROOT is set to that
    # dir so the block's "$ROOT" references resolve correctly.
    local repo_dir="$1"
    (
        set +e
        cd "$repo_dir" && ROOT="$repo_dir" bash -c '
            set -euo pipefail
            ROOT="'"$repo_dir"'"
            source "'"$DRIFT_BLOCK"'"
            echo "GATE_PASSED"
        ' 2>&1
        echo "RC=$?"
    )
}

# Fixture: bare origin + working clone, both on main.
ORIGIN="$SCRATCH_DIR/origin.git"
git init -q --bare "$ORIGIN"
WORK="$SCRATCH_DIR/work"
git clone -q "$ORIGIN" "$WORK"
(
    cd "$WORK"
    git config user.email test@test.com
    git config user.name test
    git checkout -q -b main 2>/dev/null || git checkout -q main
    echo x > f.txt
    git add f.txt
    git commit -q -m init
    git push -q origin main
)
# The bare origin's HEAD symref is set at `git init --bare` time from the
# host's init.defaultBranch (defaulting to "master" when unset). It is NOT
# automatically repointed by a later push of a "main" branch, so any
# subsequent `git clone` of $ORIGIN lands on a dangling/wrong-named unborn
# branch instead of "main" -- explicitly repoint it so later clones (e.g.
# $SECOND_CLONE below) always land on main regardless of host git config.
git --git-dir="$ORIGIN" symbolic-ref HEAD refs/heads/main

# Case 1: clean checkout on main, in sync with origin -> gate passes.
out="$(run_drift_block "$WORK")"
case "$out" in
    *GATE_PASSED*RC=0*|*RC=0*GATE_PASSED*)
        echo "PASS: drift gate passes on clean main in sync with origin"
        PASS=$((PASS + 1))
        ;;
    *)
        echo "FAIL: drift gate should pass on clean main in sync with origin. Output: $out"
        FAIL=$((FAIL + 1))
        ;;
esac

# Case 2: feature branch checked out -> gate refuses with rc=10.
(cd "$WORK" && git checkout -q -b factory/some-feature)
out="$(run_drift_block "$WORK")"
case "$out" in
    *"REFUSING TICK"*RC=10*)
        echo "PASS: drift gate refuses (rc=10) when checkout is not on main"
        PASS=$((PASS + 1))
        ;;
    *)
        echo "FAIL: drift gate should refuse (rc=10) on non-main branch. Output: $out"
        FAIL=$((FAIL + 1))
        ;;
esac
(cd "$WORK" && git checkout -q main)

# Case 3: dirty tree on main -> gate refuses with rc=10.
(cd "$WORK" && echo dirty >> f.txt)
out="$(run_drift_block "$WORK")"
case "$out" in
    *"REFUSING TICK"*RC=10*)
        echo "PASS: drift gate refuses (rc=10) on dirty working tree"
        PASS=$((PASS + 1))
        ;;
    *)
        echo "FAIL: drift gate should refuse (rc=10) on dirty tree. Output: $out"
        FAIL=$((FAIL + 1))
        ;;
esac
(cd "$WORK" && git checkout -q -- f.txt)

# Case 4: behind origin/main -> gate refuses with rc=10.
SECOND_CLONE="$SCRATCH_DIR/second"
git clone -q "$ORIGIN" "$SECOND_CLONE"
(
    cd "$SECOND_CLONE"
    git config user.email test@test.com
    git config user.name test
    echo y >> f.txt
    git add f.txt
    git commit -q -m second
    # Push via HEAD:main (not a bare "main" refspec) so this works even if
    # the clone's local branch name ever diverges from "main" for any reason.
    git push -q origin HEAD:main
)
out="$(run_drift_block "$WORK")"
case "$out" in
    *"REFUSING TICK"*RC=10*)
        echo "PASS: drift gate refuses (rc=10) when local HEAD is behind origin/main"
        PASS=$((PASS + 1))
        ;;
    *)
        echo "FAIL: drift gate should refuse (rc=10) when behind origin/main. Output: $out"
        FAIL=$((FAIL + 1))
        ;;
esac

# Case 5: AFD_SKIP_DRIFT_CHECK=1 bypasses the gate even while drifted.
out="$(
    cd "$WORK" && AFD_SKIP_DRIFT_CHECK=1 bash -c '
        set -euo pipefail
        ROOT="'"$WORK"'"
        source "'"$DRIFT_BLOCK"'"
        echo "GATE_PASSED"
    ' 2>&1
    echo "RC=$?"
)"
case "$out" in
    *GATE_PASSED*RC=0*)
        echo "PASS: AFD_SKIP_DRIFT_CHECK=1 bypasses the gate for local/dev runs"
        PASS=$((PASS + 1))
        ;;
    *)
        echo "FAIL: AFD_SKIP_DRIFT_CHECK=1 should bypass the gate. Output: $out"
        FAIL=$((FAIL + 1))
        ;;
esac

# Case 6: detached HEAD pointing to the same commit as main (in sync with origin) -> gate passes.
(
    cd "$WORK"
    git checkout -q main
    # Ensure it is in sync with origin/main
    git reset --hard -q origin/main
    # Detach HEAD at the main commit
    git checkout -q --detach HEAD
)
out="$(run_drift_block "$WORK")"
case "$out" in
    *GATE_PASSED*RC=0*|*RC=0*GATE_PASSED*)
        echo "PASS: drift gate passes on detached HEAD pointing to main commit"
        PASS=$((PASS + 1))
        ;;
    *)
        echo "FAIL: drift gate should pass on detached HEAD pointing to main commit. Output: $out"
        FAIL=$((FAIL + 1))
        ;;
esac

# Case 7: detached HEAD pointing to a different commit -> gate refuses with rc=10.
(
    cd "$WORK"
    # Detach HEAD at a different commit (e.g. the first commit, which is behind origin/main)
    # The first commit is local_sha of the init commit
    init_sha="$(git --git-dir="$ORIGIN" rev-list --max-parents=0 HEAD 2>/dev/null || true)"
    git checkout -q --detach "$init_sha"
)
out="$(run_drift_block "$WORK")"
case "$out" in
    *"REFUSING TICK"*RC=10*)
        echo "PASS: drift gate refuses (rc=10) on detached HEAD pointing to a different commit"
        PASS=$((PASS + 1))
        ;;
    *)
        echo "FAIL: drift gate should refuse (rc=10) on detached HEAD pointing to a different commit. Output: $out"
        FAIL=$((FAIL + 1))
        ;;
esac
# Restore to main for any subsequent tests
(cd "$WORK" && git checkout -q main)

# ---------------------------------------------------------------------------
# The production launchd plist must never set AFD_SKIP_DRIFT_CHECK (the
# opt-out is for local/dev invocation only).
# ---------------------------------------------------------------------------
PLIST="$ROOT/daemon/launchd/ai.dark-factory.af-tick.plist.template"
if [ -f "$PLIST" ] && grep -q AFD_SKIP_DRIFT_CHECK "$PLIST"; then
    echo "FAIL: production plist template must NOT set AFD_SKIP_DRIFT_CHECK"
    FAIL=$((FAIL + 1))
else
    echo "PASS: production plist template does not disable the drift gate"
    PASS=$((PASS + 1))
fi

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
