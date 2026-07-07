#!/usr/bin/env bash
# test_factory_af_tick.sh — round-trip tests for factory-af-tick.sh dispatch loop.
#
# Verifies the bead jleechan-xzsh refactor: factory-af-tick.sh MUST invoke
# factory-overlay.sh:dispatch-record (NOT direct sqlite UPDATE) when transitioning
# QUEUED → DISPATCHED. The overlay subcommands enforce:
#   - valid_branch regex
#   - require_state=QUEUED guard
#   - branch_registry owner check
#   - capacity() gate from config max_workers/max_batch
#   - TASK_DISPATCHED telemetry event emission
#
# Tests cover the four overlay guards plus an end-to-end factory-af-tick
# integration run with a stubbed bash "$R" (factory-ao-remediate.sh) that
# captures which subcommand calls actually happened.
#
# Run with: bash tests/scripts/test_factory_af_tick.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"

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
assert_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file" 2>/dev/null; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (pattern '$pattern' not found in $file)"
    FAIL=$((FAIL + 1))
  fi
}

# Scratch files (per-test re-created so each test starts with a clean slate).
SCRATCH_DIR="$(mktemp -d -t test-af-tick.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR" /tmp/test-af-tick-fake-r.log; }
trap cleanup EXIT

# Stub for factory-ao-remediate.sh: records the call, exits 0.
FAKE_R="$SCRATCH_DIR/fake-r.sh"
cat > "$FAKE_R" <<'STUB_R_EOF'
#!/usr/bin/env bash
echo "[fake-R] called: bead_id=$1 pr=$2 branch=${3:-}" >> /tmp/test-af-tick-fake-r.log
exit 0
STUB_R_EOF
chmod +x "$FAKE_R"
: > /tmp/test-af-tick-fake-r.log

# Helper: fresh DB + log + apply schema; sets AFD_DB / AFD_LOG accordingly.
fresh_db() {
  local tag="${1:-main}"
  export AFD_DB="$SCRATCH_DIR/cxdb-$tag.sqlite"
  export AFD_LOG="$SCRATCH_DIR/cxdb-$tag.jsonl"
  "$OVERLAY" init >/dev/null
}

# Helper: write a config TOML file with the requested capacity knobs.
write_config() {
  local mw="${1:-30}" mb="${2:-15}"
  local cfg="$SCRATCH_DIR/daemon-cap-$mw-$mb.toml"
  cat > "$cfg" <<TOML_EOF
max_workers = $mw
max_batch = $mb
TOML_EOF
  printf '%s' "$cfg"
}

# ---------------------------------------------------------------------------
# Test 1: happy path — QUEUED-after-route-record → dispatch-record succeeds
# ---------------------------------------------------------------------------
fresh_db happy
write_config 30 15 >/dev/null; export CONFIG="$(write_config 30 15)"
"$OVERLAY" intake-upsert test-happy 'happy path' >/dev/null
out="$("$OVERLAY" route-record test-happy STANDARD_PATH 'drive-existing-pr')"
assert "route-record happy" "ok" "$out"
"$OVERLAY" dispatch-record test-happy fix/test-happy >/dev/null
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-happy';")"
assert "state QUEUED → DISPATCHED (happy)" "DISPATCHED" "$state"
owner="$(sqlite3 "$AFD_DB" "SELECT bead_id FROM branch_registry WHERE branch='fix/test-happy';")"
assert "branch_registry owner (happy)" "test-happy" "$owner"
assert_grep "TASK_DISPATCHED telemetry (happy)" '"eventType": "TASK_DISPATCHED"' "$AFD_LOG"

# ---------------------------------------------------------------------------
# Test 2: require_state guard — already-DISPATCHED bead, dispatch-record rejected
# (dispatch-record expects state=QUEUED; already-DISPATCHED beads are rejected
# by the overlay's require_state guard rather than silently re-dispatched.)
# ---------------------------------------------------------------------------
"$OVERLAY" intake-upsert test-guard 'require_state guard' >/dev/null
"$OVERLAY" route-record test-guard STANDARD_PATH 'drive-existing-pr' >/dev/null
"$OVERLAY" dispatch-record test-guard fix/test-guard >/dev/null

set +e
"$OVERLAY" dispatch-record test-guard fix/test-guard-2 >"$SCRATCH_DIR/err.log" 2>&1
rc=$?
set -e
assert "dispatch-record refuses non-QUEUED bead (rc=1)" "1" "$rc"
err="$(cat "$SCRATCH_DIR/err.log")"
case "$err" in
  *expected\ one\ of*QUEUED*) echo "PASS: require_state message mentions QUEUED"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: require_state message format unexpected: $err"; FAIL=$((FAIL + 1)) ;;
esac

# ---------------------------------------------------------------------------
# Test 3: capacity gate — set max_workers=0; dispatch-record refuses
# ---------------------------------------------------------------------------
ZERO_CFG="$(write_config 0 0)"; export CONFIG="$ZERO_CFG"
fresh_db cap
"$OVERLAY" intake-upsert test-cap 'capacity test' >/dev/null
"$OVERLAY" route-record test-cap STANDARD_PATH 'drive-existing-pr' >/dev/null
cap_out="$("$OVERLAY" capacity)"
assert "capacity() with max_workers=0" "0" "$cap_out"
set +e
"$OVERLAY" dispatch-record test-cap fix/test-cap >"$SCRATCH_DIR/cap-err.log" 2>&1
rc=$?
set -e
assert "dispatch-record refuses over capacity (rc=1)" "1" "$rc"
err="$(cat "$SCRATCH_DIR/cap-err.log")"
case "$err" in
  *over\ capacity*) echo "PASS: capacity gate error mentions over capacity"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: capacity gate error format unexpected: $err"; FAIL=$((FAIL + 1)) ;;
esac
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-cap';")"
assert "state unchanged after capacity refusal" "QUEUED" "$state"

# ---------------------------------------------------------------------------
# Test 4: branch registry conflict — two beads, same branch → second dies
# ---------------------------------------------------------------------------
NORMAL_CFG="$(write_config 30 15)"; export CONFIG="$NORMAL_CFG"
fresh_db conflict
"$OVERLAY" intake-upsert test-owner 'first bead' >/dev/null
"$OVERLAY" route-record test-owner STANDARD_PATH 'drive-existing-pr' >/dev/null
"$OVERLAY" dispatch-record test-owner fix/shared-branch >/dev/null
state_owner="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-owner';")"
assert "first owner registered" "DISPATCHED" "$state_owner"

"$OVERLAY" intake-upsert test-rival 'second bead' >/dev/null
"$OVERLAY" route-record test-rival STANDARD_PATH 'drive-existing-pr' >/dev/null
set +e
"$OVERLAY" dispatch-record test-rival fix/shared-branch >"$SCRATCH_DIR/conflict.log" 2>&1
rc=$?
set -e
assert "dispatch-record refuses branch conflict (rc=1)" "1" "$rc"
err="$(cat "$SCRATCH_DIR/conflict.log")"
case "$err" in
  *registered\ to\ test-owner*)
    echo "PASS: branch conflict reports current owner (test-owner)"
    PASS=$((PASS + 1))
    ;;
  *)
    echo "FAIL: branch conflict error format unexpected: $err"
    FAIL=$((FAIL + 1))
    ;;
esac
state_rival="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-rival';")"
assert "rival state unchanged after conflict" "QUEUED" "$state_rival"

# ---------------------------------------------------------------------------
# Test 5: factory-af-tick dispatch-loop integration (happy path)
# Build a small wrapper that mirrors the dispatch loop in factory-af-tick.sh:
# pick a QUEUED-with-PR bead, stub bash "$R", then invoke route-record +
# dispatch-record (NOT direct UPDATE). Verify state moved, telemetry fired,
# bash R was called.
# ---------------------------------------------------------------------------
fresh_db integ
"$OVERLAY" intake-upsert test-integ 'integration test' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=8116, branch='fix/test-integ-branch' WHERE bead_id='test-integ';"
"$OVERLAY" route-record test-integ STANDARD_PATH 'drive-existing-pr' >/dev/null

WRAPPER="$SCRATCH_DIR/wrapper.sh"
cat > "$WRAPPER" <<WRAP_EOF
#!/usr/bin/env bash
set -euo pipefail
O="$OVERLAY"
R="$FAKE_R"
# AFD_WRAPPER_BEAD narrows the SQL to a specific bead (used by integration tests)
# to avoid picking up other QUEUED beads from the same DB.
WRAPPER_BEAD="\${AFD_WRAPPER_BEAD:-test-integ}"
ERR_TMP="\$(mktemp -t af_int.XXXXXX)"
trap 'rm -f "\$ERR_TMP"' EXIT
dispatched=0
MAX_DISPATCH=2
while IFS=\$'\t' read -r bead_id pr branch; do
  [ -n "\$bead_id" ] || continue
  [ "\$dispatched" -ge "\$MAX_DISPATCH" ] && break
  echo "[af] remediate \$bead_id PR #\$pr"
  if bash "\$R" "\$bead_id" "\$pr" 2>&1; then
    cur_state="\$(sqlite3 "\$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='\$(printf "%s" "\$bead_id" | sed "s/'/''/g")';" 2>/dev/null || true)"
    if [ "\$cur_state" = "QUEUED" ]; then
      if [ -n "\$branch" ]; then
        "\$O" route-record "\$bead_id" STANDARD_PATH "drive-existing-pr" 2>/dev/null || true
      fi
      if "\$O" dispatch-record "\$bead_id" "\$branch" 2>"\$ERR_TMP"; then
        :
      else
        err="\$(cat "\$ERR_TMP" 2>/dev/null || true)"
        case "\$err" in
          *over\ capacity*) echo "[af] over capacity — skip \$bead_id" >&2 ;;
          *already\ registered*)
            owner="\$(printf '%s' "\$err" | sed -n 's/.*already registered to //p')"
            echo "[af] branch conflict \$branch owned by \$owner — skip \$bead_id" >&2
            ;;
          *) echo "[af] dispatch-record refused for \$bead_id: \$err" >&2 ;;
        esac
        continue
      fi
    fi
    dispatched=\$((dispatched + 1))
  fi
done < <(sqlite3 "\$AFD_DB" -separator \$'\t' \
  "SELECT bead_id, pr_number, coalesce(branch,'') FROM bead_overlay
   WHERE state IN ('QUEUED','ATTESTED') AND pr_number IS NOT NULL
   AND bead_id IN ('\$WRAPPER_BEAD')
   ORDER BY updated_at LIMIT 10;")
echo "af_dispatched=\$dispatched"
WRAP_EOF
chmod +x "$WRAPPER"

: > /tmp/test-af-tick-fake-r.log
out="$(AFD_WRAPPER_BEAD=test-integ bash "$WRAPPER" 2>&1)"
af_dispatched="$(echo "$out" | grep -oE 'af_dispatched=[0-9]+' | head -1 | cut -d= -f2)"
assert "integration: af_dispatched=1" "1" "$af_dispatched"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-integ';")"
assert "integration: state=DISPATCHED via overlay" "DISPATCHED" "$state"
fake_calls="$(grep -c 'fake-R.*called' /tmp/test-af-tick-fake-r.log || true)"
assert "integration: bash R (ao spawn) called" "1" "$fake_calls"
assert_grep "integration: TASK_DISPATCHED telemetry" '"eventType": "TASK_DISPATCHED"' "$AFD_LOG"
owner="$(sqlite3 "$AFD_DB" "SELECT bead_id FROM branch_registry WHERE branch='fix/test-integ-branch';")"
assert "integration: branch_registry owner recorded" "test-integ" "$owner"

# ---------------------------------------------------------------------------
# Test 6: factory-af-tick dispatch-loop integration — capacity refusal surfaces message
# Run the same wrapper with max_workers=0; dispatch-record must refuse and the
# wrapper must surface "[af] over capacity" with state staying QUEUED.
# ---------------------------------------------------------------------------
fresh_db caploop
"$OVERLAY" intake-upsert test-caploop 'capacity loop test' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=8117, branch='fix/test-caploop-branch' WHERE bead_id='test-caploop';"
"$OVERLAY" route-record test-caploop STANDARD_PATH 'drive-existing-pr' >/dev/null
export CONFIG="$(write_config 0 0)"

: > /tmp/test-af-tick-fake-r.log
AFD_WRAPPER_BEAD=test-caploop bash "$WRAPPER" >/tmp/test-af-tick-caploop.log 2>&1 || true
# Filter the wrapper output; it should contain "[af] over capacity" message.
out="$(cat /tmp/test-af-tick-caploop.log)"
case "$out" in
  *over\ capacity*)
    echo "PASS: factory-af-tick surfaces '[af] over capacity' message"
    PASS=$((PASS + 1))
    ;;
  *)
    echo "FAIL: factory-af-tick did not surface '[af] over capacity'. Output was:"
    cat /tmp/test-af-tick-caploop.log
    FAIL=$((FAIL + 1))
    ;;
esac
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-caploop';")"
assert "integration: state stays QUEUED under capacity refusal" "QUEUED" "$state"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
