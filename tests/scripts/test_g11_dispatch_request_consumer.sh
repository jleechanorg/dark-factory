#!/usr/bin/env bash
# test_g11_dispatch_request_consumer.sh — bead jleechan-vhsw (G11).
#
# The audit (`fe-audit.sh`) emits a `DISPATCH_REQUEST` event to a side-channel
# JSONL log (`<state_dir>/dispatch_requests.jsonl`) when it finds a new ATTESTED
# bead without a DISPATCHED follow-up. The always-on `factory-af-tick.sh` is
# the consumer: it must read the side-channel log, call
# `factory-overlay.sh intake-upsert` for each bead (idempotent — beads already
# in `bead_overlay` are left alone), and atomically rotate the log so a
# partial-write mid-tick is recoverable on the next tick.
#
# Without this consumer, the audit-side signal exists but no daemon code
# acts on it — the skeptic blocked the r1 PR with exactly that finding
# ("audit writes dispatch_requests.jsonl but no daemon/tick consumer
# exists—G11 remediation loop incomplete").
#
# Tests pin 4 contract invariants against the consumer in isolation (the
# dispatch loop in `factory-af-tick.sh` is unrelated to the consumer's
# contract and is exercised separately by test_factory_af_tick.sh):
#   1. Empty log: consumer is a no-op.
#   2. Single new bead: intake-upsert called once; bead appears in
#      `bead_overlay` with state=QUEUED; the side-channel log is rotated.
#   3. Idempotent: re-running against an empty log does NOT create a
#      duplicate; a bead already in bead_overlay is left alone.
#   4. Recovery: if intake-upsert fails for one bead, OTHER beads in the
#      same log are still processed AND the failing entry stays in the log
#      for the next tick to retry (a transient downstream failure must not
#      silently drop dispatch requests).
#
# Run with: bash tests/scripts/test_g11_dispatch_request_consumer.sh
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

# Scratch area: one fresh DB + state-dir per test for isolation.
SCRATCH_DIR="$(mktemp -d -t g11-cons.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

# Set up env vars consumed by the overlay.
export AFD_DB="$SCRATCH_DIR/cxdb.sqlite"
export AFD_LOG="$SCRATCH_DIR/cxdb.jsonl"
export BR_DB="$SCRATCH_DIR/beads.db"
export FE_AUDIT_STATE_DIR="$SCRATCH_DIR/fe-audit"
mkdir -p "$FE_AUDIT_STATE_DIR"
DISPATCH_LOG="$FE_AUDIT_STATE_DIR/dispatch_requests.jsonl"

# Helper: drop a fresh schema-initialized DB + zeroed log.
fresh_db() {
  rm -f "$AFD_DB" "$DISPATCH_LOG"
  "$OVERLAY" init >/dev/null
}

# Helper: append one DISPATCH_REQUEST record (the format the audit emits).
write_dispatch_request() {
  local bid="$1" src="${2:-fe-audit-g11-intake}"
  python3 - "$bid" "$src" >> "$DISPATCH_LOG" <<PY
import json, sys
bid, src = sys.argv[1], sys.argv[2]
print(json.dumps({
    "beadId": bid,
    "eventType": "DISPATCH_REQUEST",
    "lifecycleState": "ATTESTED",
    "source": src,
}))
PY
}

# run_consumer — invokes the consumer under test with the side-channel log
# at $DISPATCH_LOG and the overlay at $OVERLAY. The consumer implementation
# lives at the path consumed by factory-af-tick.sh; the test sources it via
# a known function name (see daemon/scripts/g11_dispatch_request_consumer.sh).
CONSUMER_SH="$ROOT/daemon/scripts/g11_dispatch_request_consumer.sh"
run_consumer() {
  bash "$CONSUMER_SH" "$DISPATCH_LOG" "$OVERLAY" "$AFD_DB" 2> "$SCRATCH_DIR/cons.err"
}

# ---------------------------------------------------------------------------
# Test 1: empty log → no-op, no bead_overlay rows touched.
# ---------------------------------------------------------------------------
fresh_db
pre_count="$(sqlite3 "$AFD_DB" 'SELECT count(*) FROM bead_overlay;')"
assert "no rows initially" "0" "$pre_count"
run_consumer
post_count="$(sqlite3 "$AFD_DB" 'SELECT count(*) FROM bead_overlay;')"
assert "no rows after empty-log consumer" "0" "$post_count"
# Log file: may not even exist yet, but if it does it must be empty.
if [ -e "$DISPATCH_LOG" ]; then
  assert "log file is empty after no-op consumer" "0" "$(wc -l < "$DISPATCH_LOG" | tr -d ' ')"
else
  echo "PASS: log file absent (no audit run yet)"
  PASS=$((PASS + 1))
fi

# ---------------------------------------------------------------------------
# Test 2: one new ATTESTED bead → intake-upsert called, row exists, log rotated.
# ---------------------------------------------------------------------------
fresh_db
write_dispatch_request "jleechan-vxi3" "fe-audit-g11-intake"
write_dispatch_request "jleechan-fresh" "fe-audit-g11-intake"
pre_lines="$(wc -l < "$DISPATCH_LOG" | tr -d ' ')"
assert "two dispatch requests queued" "2" "$pre_lines"
run_consumer

vxi3_state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='jleechan-vxi3';")"
fresh_state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='jleechan-fresh';")"
assert "jleechan-vxi3 state=QUEUED after consumer" "QUEUED" "$vxi3_state"
assert "jleechan-fresh state=QUEUED after consumer" "QUEUED" "$fresh_state"

# Side-channel log must be rotated (empty / removed) so the next tick does
# not re-read the same rows. The exact rotation strategy is implementation-
# defined (truncate vs atomic rename); the contract is: no surviving entries
# for the beads we already processed.
if [ -e "$DISPATCH_LOG" ]; then
  # Count ONLY JSON lines (lines containing '"beadId"'); the consumer's
  # trailing `processed=N failures=M` summary line must not count as a
  # survivor. NOTE: `grep -c` exits 1 when the count is 0, which causes
  # `$(grep -c ... || echo 0)` to capture BOTH the grep output ("0") and
  # the fallback ("0") — wrap in `|| true` then strip the fallback.
  survivors="$(grep -c '"beadId"' "$DISPATCH_LOG" 2>/dev/null || true)"
  survivors="${survivors:-0}"
  assert "log has zero surviving DISPATCH_REQUEST entries" "0" "$survivors"
else
  echo "PASS: log rotated away (file removed)"
  PASS=$((PASS + 1))
fi

# ---------------------------------------------------------------------------
# Test 3: idempotency — re-running against an empty log must NOT create
# duplicate rows.
# ---------------------------------------------------------------------------
# bead_overlay already has jleechan-vxi3 + jleechan-fresh from Test 2.
# Drop a NEW bead + an already-known bead into a fresh log; both should land
# in bead_overlay with NO duplicate inserts and the log rotates.
write_dispatch_request "jleechan-vxi3" "fe-audit-g11-intake"   # already QUEUED
write_dispatch_request "jleechan-second" "fe-audit-g11-intake" # new
vxi3_attempts_before="$(sqlite3 "$AFD_DB" "SELECT attempt FROM bead_overlay WHERE bead_id='jleechan-vxi3';")"
run_consumer
vxi3_attempts_after="$(sqlite3 "$AFD_DB" "SELECT attempt FROM bead_overlay WHERE bead_id='jleechan-vxi3';")"
assert "idempotent intake-upsert preserves attempt counter" "$vxi3_attempts_before" "$vxi3_attempts_after"
second_state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='jleechan-second';")"
assert "new bead jleechan-second state=QUEUED" "QUEUED" "$second_state"
total_rows="$(sqlite3 "$AFD_DB" 'SELECT count(*) FROM bead_overlay;')"
assert "no duplicate inserts (3 rows)" "3" "$total_rows"

# ---------------------------------------------------------------------------
# Test 4: recovery — if intake-upsert fails for one bead, OTHER beads in the
# same log are still processed AND the failing entry stays so the next tick
# retries it. A transient downstream failure must not silently drop requests.
#
# We simulate the failure by pointing the consumer at an overlay that
# rejects the failing bead id. We use a tiny wrapper overlay shim that
# forwards valid_bead_id-allowlisted ids to the real overlay and rejects
# everything else — same contract as the real overlay's `valid_bead_id`
# guard, which rejects beads whose id contains chars outside
# `[A-Za-z0-9._-]`. A bead like `bad id with spaces` will fail.
# ---------------------------------------------------------------------------
SHIM_OVERLAY="$SCRATCH_DIR/overlay-shim.sh"
cat > "$SHIM_OVERLAY" <<'SHIM_EOF'
#!/usr/bin/env bash
# Shim: forwards intake-upsert to the real overlay ONLY when the bead_id
# matches the allowlist; rejects everything else. Mirrors the real
# overlay's valid_bead_id regex.
set -euo pipefail
REAL_OVERLAY="${SHIM_REAL_OVERLAY:-}"
if [ "$1" = "intake-upsert" ]; then
  bid="$2"
  case "$bid" in
    *' '*|*"'"*) echo "rejected: invalid bead_id: $bid" >&2; exit 22 ;;
  esac
  case "$bid" in
    *[!A-Za-z0-9._-]*) echo "rejected: invalid bead_id: $bid" >&2; exit 22 ;;
  esac
  exec "$REAL_OVERLAY" "$@"
fi
exec "$REAL_OVERLAY" "$@"
SHIM_EOF
chmod +x "$SHIM_OVERLAY"
export SHIM_REAL_OVERLAY="$OVERLAY"

fresh_db
write_dispatch_request "jleechan-recover-ok" "fe-audit-g11-intake"
write_dispatch_request "bad id with spaces" "fe-audit-g11-intake"  # overlay rejects
bash "$CONSUMER_SH" "$DISPATCH_LOG" "$SHIM_OVERLAY" "$AFD_DB" 2> "$SCRATCH_DIR/cons.err"

ok_state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='jleechan-recover-ok';")"
assert "valid bead still processed despite sibling failure" "QUEUED" "$ok_state"

# The contract under test: the failing entry must NOT be silently dropped.
# Either it stays in the log OR the consumer re-emits it on the next tick.
# We accept either as long as the bead id appears somewhere observable
# after the failed tick.
if grep -q '"bad id with spaces"' "$DISPATCH_LOG" 2>/dev/null; then
  echo "PASS: failing entry preserved in log for next-tick retry"
  PASS=$((PASS + 1))
else
  echo "FAIL: failing entry was silently dropped"
  FAIL=$((FAIL + 1))
fi

echo
echo "---- consumer stderr ----"
cat "$SCRATCH_DIR/cons.err" || true
echo "-------------------------"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
