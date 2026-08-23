#!/usr/bin/env bash
# test_fe_audit_cancelled.sh — regression test for G11 accounting excluding CANCELLED state.
#
# Verifies:
# 1. bash -n syntax validation of daemon/scripts/fe-audit.sh
# 2. python syntax validation of daemon/scripts/fe_audit_query.py
# 3. fe_audit_query.py g11_cancelled output matches beads with lifecycleState=CANCELLED
# 4. fe-audit.sh subtracts CANCELLED beads alongside HUMAN_HELD from G11 stuck count
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
FE_AUDIT="$ROOT/daemon/scripts/fe-audit.sh"
QUERY_PY="$ROOT/daemon/scripts/fe_audit_query.py"

PASS=0; FAIL=0
assert_eq() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"; FAIL=$((FAIL + 1))
  fi
}

# 1. Syntax checks
if bash -n "$FE_AUDIT"; then
  echo "PASS: bash -n parses fe-audit.sh"; PASS=$((PASS + 1))
else
  echo "FAIL: bash -n rejects fe-audit.sh"; FAIL=$((FAIL + 1))
fi

if python3 -m py_compile "$QUERY_PY"; then
  echo "PASS: python3 -m py_compile parses fe_audit_query.py"; PASS=$((PASS + 1))
else
  echo "FAIL: python3 -m py_compile rejects fe_audit_query.py"; FAIL=$((FAIL + 1))
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

LOG_FILE="$TMP_DIR/daemon.jsonl"
STATE_DIR="$TMP_DIR/state"
mkdir -p "$STATE_DIR"

NOW_ISO="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# Scenario 1: Telemetry with attested bead cancelled -> STUCK_COUNT must be 0
cat > "$LOG_FILE" <<JSONL
{"timestamp": "$NOW_ISO", "eventType": "TICK"}
{"timestamp": "$NOW_ISO", "beadId": "bead-c1", "lifecycleState": "ATTESTED", "eventType": "INTAKE"}
{"timestamp": "$NOW_ISO", "beadId": "bead-c1", "lifecycleState": "CANCELLED", "eventType": "SKIPPED_DUPLICATE_BEAD"}
{"timestamp": "$NOW_ISO", "beadId": "bead-h1", "lifecycleState": "ATTESTED", "eventType": "INTAKE"}
{"timestamp": "$NOW_ISO", "beadId": "bead-h1", "lifecycleState": "HUMAN_HELD", "eventType": "HOLD"}
JSONL

OUT_QUERY="$(python3 "$QUERY_PY" g11_cancelled "$LOG_FILE" "2000-01-01T00:00:00Z")"
assert_eq "g11_cancelled finds bead-c1" "bead-c1" "$OUT_QUERY"

OUT_AUDIT="$(FE_AUDIT_LOG="$LOG_FILE" FE_AUDIT_STATE_DIR="$STATE_DIR" LOOKBACK_HOURS=24 MAX_TICK_GAP_SEC=86400 /bin/bash "$FE_AUDIT" --no-bead)"
STUCK_LINE="$(echo "$OUT_AUDIT" | grep "G11: attested=" | head -1)"
assert_eq "audit reports 0 stuck beads when all un-dispatched are cancelled or held" \
  "[fe-audit $(echo "$STUCK_LINE" | awk '{print $2}') G11: attested=0 (no DISPATCHED follow-up over 24h)" \
  "$STUCK_LINE"

# Scenario 2: Telemetry with 1 genuine stuck bead and 1 cancelled bead -> STUCK_COUNT must be 1
cat >> "$LOG_FILE" <<JSONL
{"timestamp": "$NOW_ISO", "beadId": "bead-stuck", "lifecycleState": "ATTESTED", "eventType": "INTAKE"}
JSONL

OUT_AUDIT_2="$(FE_AUDIT_LOG="$LOG_FILE" FE_AUDIT_STATE_DIR="$STATE_DIR" LOOKBACK_HOURS=24 MAX_TICK_GAP_SEC=86400 /bin/bash "$FE_AUDIT" --no-bead)"
STUCK_LINE_2="$(echo "$OUT_AUDIT_2" | grep "G11: attested=" | head -1)"
assert_eq "audit reports exactly 1 stuck bead when 1 is genuinely stuck and 1 is cancelled" \
  "[fe-audit $(echo "$STUCK_LINE_2" | awk '{print $2}') G11: attested=1 (no DISPATCHED follow-up over 24h)" \
  "$STUCK_LINE_2"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
