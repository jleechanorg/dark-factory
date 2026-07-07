#!/usr/bin/env bash
# test_factory_af_tick_structured_rc.sh — verifies the structured exit-code
# contract between daemon/factory-af-tick.sh and daemon/factory-overlay.sh.
#
# ZFC-correct dispatch (per bead jleechan-q47c, jleechan-81wa, jleechan-2r1k):
# each failure class has its own exit code; callers case on $rc, NOT stderr.
#
# Run with: bash tests/scripts/test_factory_af_tick_structured_rc.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"
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

# ---------- factory-af-tick.sh arg validation ----------
set +e
( cd /tmp && bash "$TICK" --prs "1,2,abc" ) >/dev/null 2>&1
rc=$?
set -e
assert "factory-af-tick --prs rejects non-numeric (rc=2)" "2" "$rc"

set +e
( cd /tmp && AFD_BEAD_FILTER='jleechan-test;rm -rf /' bash "$TICK" ) >/dev/null 2>&1
rc=$?
set -e
assert "factory-af-tick AFD_BEAD_FILTER rejects shell meta (rc=2)" "2" "$rc"

set +e
( cd /tmp && bash "$TICK" --bogus ) >/dev/null 2>&1
rc=$?
set -e
assert "factory-af-tick rejects unknown arg (rc=2)" "2" "$rc"

# ---------- factory-overlay.sh structured exit codes ----------
SCRATCH_DIR="$(mktemp -d -t test-af-tick-rc.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

export AFD_DB="$SCRATCH_DIR/cxdb.sqlite"
export AFD_LOG="$SCRATCH_DIR/cxdb.jsonl"
export CONFIG="$SCRATCH_DIR/cfg.toml"

cat > "$CONFIG" <<TOML
max_workers = 30
max_batch = 15
TOML

"$OVERLAY" init >/dev/null

# Branch conflict: rc=4 (EX_BRANCH_CONFLICT)
"$OVERLAY" intake-upsert jleechan-test 'test bead' >/dev/null
"$OVERLAY" route-record jleechan-test STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record jleechan-test fix/test-branch >/dev/null
"$OVERLAY" intake-upsert jleechan-other 'another bead' >/dev/null
"$OVERLAY" route-record jleechan-other STANDARD_PATH >/dev/null
set +e
"$OVERLAY" dispatch-record jleechan-other fix/test-branch >/dev/null 2>&1
rc=$?
set -e
assert "dispatch-record branch conflict (rc=4 EX_BRANCH_CONFLICT)" "4" "$rc"

# Over capacity: rc=3 (EX_OVER_CAP)
cat > "$CONFIG" <<TOML
max_workers = 0
max_batch = 0
TOML
"$OVERLAY" intake-upsert jleechan-cap 'cap test' >/dev/null
"$OVERLAY" route-record jleechan-cap STANDARD_PATH >/dev/null
set +e
"$OVERLAY" dispatch-record jleechan-cap fix/cap-test >/dev/null 2>&1
rc=$?
set -e
assert "dispatch-record over capacity (rc=3 EX_OVER_CAP)" "3" "$rc"

# require_state: rc=5 (EX_REQUIRE_STATE)
cat > "$CONFIG" <<TOML
max_workers = 30
max_batch = 15
TOML
"$OVERLAY" intake-upsert jleechan-twice 'twice' >/dev/null
"$OVERLAY" route-record jleechan-twice STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record jleechan-twice fix/twice >/dev/null
set +e
"$OVERLAY" dispatch-record jleechan-twice fix/twice-other >/dev/null 2>&1
rc=$?
set -e
assert "dispatch-record require_state (rc=5 EX_REQUIRE_STATE)" "5" "$rc"

# Invalid bead_id: rc=7 (EX_BEAD_ID)
set +e
"$OVERLAY" intake-upsert "bad'bead;id" 'invalid' >/dev/null 2>&1
rc=$?
set -e
assert "intake-upsert invalid bead_id (rc=7 EX_BEAD_ID)" "7" "$rc"

# Invalid branch format: rc=6 (EX_VALID_INPUT)
set +e
"$OVERLAY" dispatch-record jleechan-test "bad branch with spaces &!" >/dev/null 2>&1
rc=$?
set -e
assert "dispatch-record invalid branch (rc=6 EX_VALID_INPUT)" "6" "$rc"

# Usage error: rc=2 (EX_USAGE)
set +e
"$OVERLAY" dispatch-record >/dev/null 2>&1
rc=$?
set -e
assert "dispatch-record missing args (rc=2 EX_USAGE)" "2" "$rc"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1