#!/usr/bin/env bash
# test_fe_audit.sh — unit and integration tests for fe-audit.sh and fe_audit_query.py
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FE_AUDIT="$REPO_ROOT/daemon/scripts/fe-audit.sh"
QUERY_PY="$REPO_ROOT/daemon/scripts/fe_audit_query.py"

TMP_DIR="$(mktemp -d)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

PASS=0
FAIL=0

assert_eq() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"
    FAIL=$((FAIL + 1))
  fi
}

echo "=== Test 1: fe_audit_query.py g11_cancelled ==="
LOG_1="$TMP_DIR/log1.jsonl"
cat > "$LOG_1" <<'JSONL'
{"timestamp":"2026-08-22T10:00:00Z","beadId":"bead-c1","lifecycleState":"CANCELLED"}
{"timestamp":"2026-08-22T11:00:00Z","beadId":"bead-c2","lifecycleState":"CANCELLED"}
{"timestamp":"2026-08-22T11:30:00Z","beadId":"bead-c1","lifecycleState":"CANCELLED"}
{"timestamp":"2026-08-20T10:00:00Z","beadId":"bead-old","lifecycleState":"CANCELLED"}
{"timestamp":"2026-08-22T10:00:00Z","beadId":"bead-held","lifecycleState":"HUMAN_HELD"}
JSONL

CUTOFF="2026-08-22T00:00:00Z"
RES_1="$(python3 "$QUERY_PY" g11_cancelled "$LOG_1" "$CUTOFF" 2>/dev/null || echo "ERROR")"
EXPECTED_1=$(printf "bead-c1\nbead-c2")
assert_eq "g11_cancelled returns unique matching beads after cutoff" "$EXPECTED_1" "$RES_1"

echo "=== Test 2: fe-audit.sh G11 accounting excludes CANCELLED beads ==="
LOG_2="$TMP_DIR/log2.jsonl"
STATE_DIR_2="$TMP_DIR/state2"
mkdir -p "$STATE_DIR_2"

NOW_ISO="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
# Create beads:
# - bead-normal: ATTESTED and DISPATCHED (not stuck)
# - bead-held: ATTESTED and HUMAN_HELD (excluded)
# - bead-cancelled: ATTESTED and CANCELLED (excluded)
# - bead-stuck: ATTESTED only (STUCK!)
cat > "$LOG_2" <<JSONL
{"eventType":"TICK","timestamp":"$NOW_ISO"}
{"timestamp":"$NOW_ISO","beadId":"bead-normal","lifecycleState":"ATTESTED"}
{"timestamp":"$NOW_ISO","beadId":"bead-normal","lifecycleState":"DISPATCHED"}
{"timestamp":"$NOW_ISO","beadId":"bead-held","lifecycleState":"ATTESTED"}
{"timestamp":"$NOW_ISO","beadId":"bead-held","lifecycleState":"HUMAN_HELD"}
{"timestamp":"$NOW_ISO","beadId":"bead-cancelled","lifecycleState":"ATTESTED"}
{"timestamp":"$NOW_ISO","beadId":"bead-cancelled","lifecycleState":"CANCELLED"}
JSONL

OUT_2="$(FE_AUDIT_STATE_DIR="$STATE_DIR_2" bash "$FE_AUDIT" --no-bead --log "$LOG_2" --lookback 24 2>&1)"
echo "$OUT_2"
assert_eq "G11 reports 0 stuck beads when non-dispatched beads are CANCELLED or HUMAN_HELD" "true" "$(echo "$OUT_2" | grep -q "G11: attested=0" && echo "true" || echo "false")"

echo "=== Test 3: fe-audit.sh G11 accounting detects genuine stuck beads ==="
cat >> "$LOG_2" <<JSONL
{"timestamp":"$NOW_ISO","beadId":"bead-genuinely-stuck","lifecycleState":"ATTESTED"}
JSONL

OUT_3="$(FE_AUDIT_STATE_DIR="$STATE_DIR_2" bash "$FE_AUDIT" --no-bead --log "$LOG_2" --lookback 24 2>&1)"
echo "$OUT_3"
assert_eq "G11 reports 1 stuck bead when an ATTESTED bead is not dispatched/held/cancelled" "true" "$(echo "$OUT_3" | grep -q "G11: attested=1" && echo "true" || echo "false")"

echo "=========================================="
echo "Summary: $PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
