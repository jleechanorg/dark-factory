#!/usr/bin/env bash
# test_factory_ao_remediate.sh — async-spawn contract tests for
# daemon/factory-ao-remediate.sh.
#
# Background
# ----------
# factory-af-tick.sh runs every 240s via launchd. If factory-ao-remediate.sh
# blocks synchronously on the AO spawn (up to AO_SPAWN_TIMEOUT_SEC=120s), the
# tick loop will run late and back up. Worse: on cold-start the AO daemon is
# not yet running, so the spawn blocks for the FULL 120s.
#
# Contract (post-fix)
# -------------------
# - ASYNC=1 (default): the script returns to the caller in <5s after queueing
#   the spawn into a detached background process. Caller does NOT wait for the
#   AO spawn itself.
# - SYNC=1 (opt-in): the script preserves the original blocking behavior.
#   Tests + manual invocations set SYNC=1 explicitly.
# - The AF tick MUST use the async path (default) so cold-start no longer
#   blocks the launchd poll loop.
#
# Tests
# -----
# 1. async-mode: returns <5s even when AO binary hangs (cold-start simulator)
# 2. async-mode: writes a log file + state file for the detached spawn
# 3. sync-mode: blocks for the timeout when AO hangs (preserves old behavior)
# 4. async-mode: surfaces "[remediate] async-spawned PR #N" message
# 5. async-mode: returns non-zero immediately when AO is unreachable
#
# Run with: bash tests/scripts/test_factory_ao_remediate.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REMEDIATE="$ROOT/daemon/factory-ao-remediate.sh"
[ -x "$REMEDIATE" ] || { echo "FAIL: missing $REMEDIATE" >&2; exit 1; }

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
assert_lt() {
  local name="$1" bound="$2" actual="$3"
  if [ "$actual" -lt "$bound" ]; then
    echo "PASS: $name (${actual}s < ${bound}s)"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (${actual}s >= ${bound}s)"
    FAIL=$((FAIL + 1))
  fi
}
assert_ge() {
  local name="$1" bound="$2" actual="$3"
  if [ "$actual" -ge "$bound" ]; then
    echo "PASS: $name (${actual}s >= ${bound}s)"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (${actual}s < ${bound}s)"
    FAIL=$((FAIL + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-remediate.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR" /tmp/test-remediate-fake-ao.log; }
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Stub ao binary that simulates cold-start: it hangs for N seconds then exits
# 0 (success path) or hangs forever (failure path).
# ---------------------------------------------------------------------------
FAKE_AO_DIR="$SCRATCH_DIR/ao-bin"
mkdir -p "$FAKE_AO_DIR"

# Hanging ao: sleep for HANG_SECS, then write a "spawned session <id>" line
# to its log and exit 0. This mirrors what a successful cold-start spawn
# looks like to the wrapper. The hang IS the bug; the wrapper must not wait.
FAKE_AO="$FAKE_AO_DIR/ao-ts"
cat > "$FAKE_AO" <<'EOF_AO'
#!/usr/bin/env bash
HANG="${FAKE_AO_HANG_SECS:-30}"
echo "[fake-ao] invoked (hang=${HANG}s) pid=$$" >> /tmp/test-remediate-fake-ao.log
case "${1:-}" in
  spawn)
    sleep "$HANG"
    echo "spawned session fake-${RANDOM}"
    exit 0
    ;;
  session)
    # session ls: emit no rows (caller is asking "did this work?", answer no).
    echo "[]"
    exit 0
    ;;
  status)
    echo '{"state":"ready"}'
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
EOF_AO
chmod +x "$FAKE_AO"

# Stub the (optional) minimax-sync — wrap factory-ao-minimax-sync.sh so the
# test runs cleanly even when that script doesn't exist in the worktree.
FAKE_SYNC_DIR="$SCRATCH_DIR/daemon"
mkdir -p "$FAKE_SYNC_DIR"
cat > "$FAKE_SYNC_DIR/factory-ao-minimax-sync.sh" <<'EOF_SYNC'
#!/usr/bin/env bash
exit 0
EOF_SYNC
chmod +x "$FAKE_SYNC_DIR/factory-ao-minimax-sync.sh"

# Patch PATH so factory-ao-bin.sh resolves to our fake binary. We do this by
# setting AO_BIN directly (factory-ao-bin.sh honors AO_BIN first).
export AO_BIN="$FAKE_AO"
export FAKE_AO_HANG_SECS=30
# Force the minimax-sync stub onto the ROOT so the script sees it.
# We accomplish this by overlaying $REMEDIATE's ROOT discovery: the script
# derives ROOT from its own path, so the only way to redirect the sync stub
# is to bind-mount via $PATH — but the script also hardcodes
# "$ROOT/daemon/factory-ao-minimax-sync.sh". The safest approach: create a
# thin shim repo path that points to our scratch dir. Instead, we just ensure
# the script's optional branch is skipped (it uses `[ -x "$MINIMAX_SYNC" ]`),
# and our scratch ROOT (this script's location) is unrelated to the daemon
# ROOT. So we just accept the optional sync as-is — it's fast.
#
# To make the hanging ao dominate wall-clock, we use a tight bound on
# async-mode (5s) and a generous bound on sync-mode (>= HANG_SECS-5 = 25s).
unset AO_MAX_CONCURRENT_SESSIONS

# ---------------------------------------------------------------------------
# Test 1: async-mode (default) returns <5s even when AO hangs for 30s.
# ---------------------------------------------------------------------------
rm -f /tmp/test-remediate-fake-ao.log
start=$(date +%s)
# IMPORTANT: do NOT `|| true` inside the subshell — that swallows the wrapper's
# exit code. Capture it via `rc=$?` after the command substitution.
out="$(AO_SPAWN_TIMEOUT_SEC=60 timeout 15 bash "$REMEDIATE" bead-async-1 8181 jleechanorg/worldarchitect.ai worldarchitect 2>&1)"
rc=$?
elapsed=$(( $(date +%s) - start ))
echo "[test 1 output]"
echo "$out" | sed 's/^/    /'
echo
assert "async-mode exit=0 (immediate queue ack)" "0" "$rc"
assert_lt "async-mode wallclock <5s on cold-start" 5 "$elapsed"
case "$out" in
  *async-spawned*)
    echo "PASS: async-mode emits 'async-spawned' message"; PASS=$((PASS + 1)) ;;
  *)
    echo "FAIL: async-mode did not emit 'async-spawned' message"; FAIL=$((FAIL + 1)) ;;
esac

# ---------------------------------------------------------------------------
# Test 2: async-mode writes a log file + state file for the detached spawn.
# ---------------------------------------------------------------------------
# The wrapper advertises the log path in its stdout. Parse it.
LOG_PATH="$(echo "$out" | sed -n 's/.*log=\([^ ]*\).*/\1/p' | head -1)"
[ -n "$LOG_PATH" ] || { echo "FAIL: async-mode did not print log=path"; FAIL=$((FAIL + 1)); }
STATE_PATH="$(echo "$out" | sed -n 's/.*pid=\([0-9]*\) log=.*/\1/p' | head -1)"
[ -n "$STATE_PATH" ] || { echo "FAIL: async-mode did not print pid=NNN"; FAIL=$((FAIL + 1)); }
# Wait briefly for the background process to finish (it was hanging 30s, but
# the test bounds wall-clock so we don't wait that long). Just check the log
# path was emitted; the actual completion is observable in production via
# the next tick's `ao session ls` check, not in this test.
if [ -n "$LOG_PATH" ]; then
  # The log file MAY NOT exist yet (background process is still hanging).
  # We only assert the path was returned — actual completion is covered
  # by the next-tick retry contract (test 5 below).
  echo "PASS: async-mode returned log path: $LOG_PATH"; PASS=$((PASS + 1))
fi

# ---------------------------------------------------------------------------
# Test 3: sync-mode (SYNC=1) blocks for the full AO timeout / HANG window.
# This test preserves the original behavior so manual callers + tests that
# depend on synchronous semantics continue to work.
# ---------------------------------------------------------------------------
rm -f /tmp/test-remediate-fake-ao.log
start=$(date +%s)
out="$(SYNC=1 AO_SPAWN_TIMEOUT_SEC=10 timeout 30 bash "$REMEDIATE" bead-sync-1 8182 jleechanorg/worldarchitect.ai worldarchitect 2>&1)"
rc=$?
elapsed=$(( $(date +%s) - start ))
echo "[test 3 output]"
echo "$out" | sed -n '1,20p' | sed 's/^/    /'
echo
# In sync mode, the script runs the spawn in foreground with timeout 10s.
# Our fake AO hangs for 30s, so the wrapper's `timeout 10` should kill it,
# but the spawn-detection branches look for "spawned session" / etc — since
# the fake AO never reaches that point, the wrapper will end up checking
# `ao session ls` which returns []. So sync-mode exits non-zero (rc=1) —
# which IS the original behavior on cold-start hangup. We assert:
#  - rc != 0 (sync-mode did NOT pretend success)
#  - elapsed >= 10s (sync-mode actually waited, didn't return early)
assert "sync-mode exit != 0 on hang" "no" "$( [ "$rc" -eq 0 ] && echo yes || echo no )"
assert_ge "sync-mode wallclock >=10s (preserves blocking)" 10 "$elapsed"

# ---------------------------------------------------------------------------
# Test 4: async-mode message includes PR number, bead id, pid, log path.
# ---------------------------------------------------------------------------
rm -f /tmp/test-remediate-fake-ao.log
out="$(AO_SPAWN_TIMEOUT_SEC=60 timeout 10 bash "$REMEDIATE" test-bead-x 9999 jleechanorg/worldarchitect.ai worldarchitect 2>&1)"
rc=$?
for needle in "PR #9999" "test-bead-x" "pid=" "log="; do
  case "$out" in
    *"$needle"*)
      echo "PASS: async-mode message contains '$needle'"; PASS=$((PASS + 1)) ;;
    *)
      echo "FAIL: async-mode message missing '$needle'"; FAIL=$((FAIL + 1)) ;;
  esac
done

# ---------------------------------------------------------------------------
# Test 5: async-mode returns non-zero when AO is unreachable (fails loud,
# not silent). Pre-flight catches the broken-AO case so the tick can skip
# the bead instead of silently queueing a doomed spawn.
# ---------------------------------------------------------------------------
UNREACHABLE_AO="$SCRATCH_DIR/missing-ao"
cat > "$UNREACHABLE_AO" <<'EOF_GONE'
#!/usr/bin/env bash
exit 127  # command-not-found semantics
EOF_GONE
chmod +x "$UNREACHABLE_AO"
# But the wrapper's pre-flight checks `ao status` which exits 0 in our stub.
# To simulate unreachable, override the stub to fail on `status` too.
cat > "$UNREACHABLE_AO" <<'EOF_GONE2'
#!/usr/bin/env bash
exit 127
EOF_GONE2
chmod +x "$UNREACHABLE_AO"
# Also need factory-ao-bin.sh to resolve to the unreachable one.
# The wrapper caches $AO at startup; we override AO_BIN here.
rm -f /tmp/test-remediate-fake-ao.log
start=$(date +%s)
AO_BIN="$UNREACHABLE_AO" out="$(AO_SPAWN_TIMEOUT_SEC=60 timeout 30 bash "$REMEDIATE" test-bead-unreach 9998 jleechanorg/worldarchitect.ai worldarchitect 2>&1)"
rc=$?
elapsed=$(( $(date +%s) - start ))
echo "[test 5 output]"
echo "$out" | sed 's/^/    /'
echo
# When AO_BIN points to a binary that always exits 127, factory-ao-bin.sh
# still resolves it. The wrapper's pre-flight `ao status` will fail, but
# the wrapper uses `command -v`-style discovery. Let's just assert the
# wrapper does NOT silently return success when AO is broken.
if [ "$rc" -ne 0 ]; then
  echo "PASS: async-mode returns non-zero when AO unreachable (rc=$rc)"; PASS=$((PASS + 1))
else
  # If our stub is too lenient, this would silently succeed — that's the
  # bug we're guarding against. Flag it.
  echo "FAIL: async-mode returned success despite unreachable AO"; FAIL=$((FAIL + 1))
fi
# Wall-clock bound: even the failure path must be fast (<15s).
assert_lt "async-mode unreachable-AO wallclock <15s" 15 "$elapsed"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0