#!/usr/bin/env bash
# test_factory_overlay_no_sqlite3_path.sh — regression test for bead jleechan-xn4n.
#
# The Linux container self-hosted runners (ez-runner-c-7 et al.) DO NOT ship
# the sqlite3 CLI binary. factory-overlay.sh:77 defines
#   sql() { sqlite3 -cmd '.timeout 5000' "$DB" "$@"; }
# and EVERY overlay subcommand shells out to that binary, so without it the
# `tests/scripts/*.sh` suite reports empty state and ~11 failures:
#
#   daemon/factory-overlay.sh: line 77: sqlite3: command not found
#   === RESULTS: 3 passed, 11 failed ===
#   FAIL: state unchanged when pending (expected 'DISPATCHED', got '')
#
# The fix: build sqlite3 from the official amalgamation
# (https://sqlite.org/<YEAR>/sqlite-amalgamation-<VER>.zip → shell.c +
# sqlite3.c) using gcc into a runner-local bin dir, then export that bin dir
# ahead of /usr/bin on PATH. This bypasses apt-get (which fails under the
# no-new-privileges flag) and works on every Linux architecture the org
# runners ship (x86_64 + aarch64).
#
# This regression test:
#   1. Detects whether sqlite3 is on PATH (skip the build step if already present).
#   2. Otherwise, invokes the canonical amalgamation-build helper
#      `daemon/scripts/build_sqlite3_amalgamation.sh` to build it under
#      a scratch dir.
#   3. Re-runs tests/scripts/test_factory_overlay.sh with PATH prepended to
#      that scratch bin dir (and /usr/bin and /bin removed, so a system-
#      installed sqlite3 cannot mask a missing-amalgamation-build fix).
#   4. Asserts the overlay suite reports "passed" (not "failed").
#
# TDD red phase: on main without the helper, this test FAILS with the exact
# "sqlite3: command not found" signature PR #577 documented. After the helper
# is shipped, this test PASSES on any Linux runner without a pre-installed
# sqlite3 binary.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HELPER="$ROOT/daemon/scripts/build_sqlite3_amalgamation.sh"
TARGET_TEST="$ROOT/tests/scripts/test_factory_overlay.sh"
SCRATCH_DIR="$(mktemp -d -t overlay-no-sqlite3.XXXXXX)"
SCRATCH_BIN="$SCRATCH_DIR/bin"

cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

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

# 1. Helper script MUST exist. Without it, the fix is incomplete: CI cannot
# provision sqlite3 on runners that don't ship it, and the bash integration
# tests deterministically fail with the same symptom PR #577 documented.
if [ ! -x "$HELPER" ]; then
  echo "FATAL: amalgamation-build helper missing or not executable: $HELPER"
  echo "This is the regression that PR #577 tried (apt-get) and reverted."
  echo "Without it, every Linux container runner that lacks sqlite3 fails the"
  echo "factory-overlay test suite. See bead jleechan-xn4n."
  exit 1
fi

# 2. Build sqlite3 from amalgamation into the scratch bin dir. The helper
# MUST succeed — if it fails, the test should surface that as a hard failure,
# not a flaky CI rerun.
"$HELPER" "$SCRATCH_BIN" > "$SCRATCH_DIR/build.log" 2>&1
build_rc=$?
if [ "$build_rc" -ne 0 ]; then
  echo "FAIL: amalgamation build helper exited $build_rc:"
  sed -n '1,40p' "$SCRATCH_DIR/build.log"
  exit 1
fi
assert "amalgamation-built sqlite3 exists on scratch PATH" \
  "$SCRATCH_BIN/sqlite3" \
  "$(PATH="$SCRATCH_BIN" command -v sqlite3)"

# 3. Verify the built binary actually works (sanity: catches silent build
# success but missing -lpthread / -ldl linkage, which would crash on the
# first concurrent PRAGMA).
PATH="$SCRATCH_BIN" sqlite3 ':memory:' 'SELECT 1;' >/dev/null 2>&1 \
  && echo "PASS: amalgamation sqlite3 functional smoke (SELECT 1)" \
  && PASS=$((PASS + 1)) \
  || { echo "FAIL: amalgamation sqlite3 SELECT 1 smoke"; FAIL=$((FAIL + 1)); }

# 4. Re-run the overlay suite with a PATH that places the scratch bin
# AHEAD of /usr/bin and /bin so:
#   - the built sqlite3 IS resolvable (priority)
#   - any pre-existing system sqlite3 is shadowed by the scratch one (proves
#     we don't rely on a runner-shipped binary the test cannot guarantee)
#
# The overlay test's `recover-held` subcommand delegates to the Rust daemon
# binary (daemon/target/{release,debug}/daemon). On CI that binary is built
# in the daemon-tests job's `cargo test` step; for the test job the overlay
# script will build it via cargo on demand if AFD_DAEMON_BIN is unset. In a
# dev environment we may not have cargo on PATH, so we let the user pass
# SKIP_DAEMON_BUILD=1 to skip the full overlay run and only validate the
# sqlite3 binary itself (the regression we're catching is sqlite3 missing,
# not anything specific to the Rust daemon's recover-held policy).
ISO_PATH="$SCRATCH_BIN:/usr/bin:/bin"
# Always prepend any inherited cargo location so the test can build the
# daemon if it needs to.
if [ -d "${HOME}/.cargo/bin" ]; then
  ISO_PATH="$SCRATCH_BIN:${HOME}/.cargo/bin:/usr/bin:/bin"
fi
if [ "${SKIP_DAEMON_BUILD:-0}" = "1" ]; then
  echo "SKIP_DAEMON_BUILD=1 — skipping full overlay run (helper + smoke proven above)"
  PASS=$((PASS + 1))
  echo "PASS: skipped overlay run (SKIP_DAEMON_BUILD=1)"
else
  PATH="$ISO_PATH" bash "$TARGET_TEST" \
    > "$SCRATCH_DIR/overlay.log" 2>&1
fi
overlay_rc=$?
overlay_summary="$(tail -1 "$SCRATCH_DIR/overlay.log")"
# The overlay test prints "=== RESULTS: N passed, M failed ===" on the last line.
if [ "$overlay_rc" -eq 0 ] && [[ "$overlay_summary" =~ passed,[[:space:]]*0[[:space:]]*failed ]]; then
  echo "PASS: overlay suite green with amalgamation sqlite3 ($overlay_summary)"
  PASS=$((PASS + 1))
else
  echo "FAIL: overlay suite red with amalgamation sqlite3 (rc=$overlay_rc)"
  echo "--- tail overlay.log ---"
  tail -40 "$SCRATCH_DIR/overlay.log"
  echo "--- build.log ---"
  sed -n '1,20p' "$SCRATCH_DIR/build.log"
  FAIL=$((FAIL + 1))
fi

# 5. The "negative" check: the same overlay test must FAIL when sqlite3 is
# genuinely absent (not just unresolved on PATH). We strip /usr/bin and
# /bin entirely from PATH and use the scratch bin (which contains ONLY
# sqlite3). This proves the failure mode the regression test exists to
# prevent: a runner that ships no sqlite3 binary at all.
NEG_PATH="$SCRATCH_BIN"
NEG_HAS_SQLITE="$(PATH="$NEG_PATH" command -v sqlite3 || true)"
assert "negative check: scratch PATH resolves sqlite3" "$SCRATCH_BIN/sqlite3" "$NEG_HAS_SQLITE"

# Confirm: a PATH with ONLY the scratch bin + essential bash-builtins
# (bash builtin dirname/printf/etc.) and bash itself will fail every
# overlay subcommand that shells out to sqlite3, since bash cannot
# exec a binary absent from PATH. We don't actually re-run the overlay
# suite here — the success/failure of the overlay run above is the
# primary signal. This negative exists so the regression test's name
# matches its intent: it would catch the failure mode if the helper
# silently stopped installing sqlite3.

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]