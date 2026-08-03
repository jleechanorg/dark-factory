#!/usr/bin/env bash
# Smoke test: verify the G11 fe-audit subtracts the new DISPATCH_REQUEST
# telemetry event alongside the pre-existing TASK_DISPATCHED flow.
#
# Bead jleechan-7lom / G11 startup-intake-without-forced-dispatch:
# drives the actual fe-audit.sh pipeline against a synthetic daemon.jsonl
# that contains an ATTESTED bead with a paired DISPATCH_REQUEST event.
# Asserts the G11 check reports 0 stuck beads (because the DISPATCH_REQUEST
# pair cancels the ATTESTED entry).
#
# Run: bash tests/scripts/test_fe_audit_g11_dispatch_request_pair.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
FE_AUDIT="$ROOT/daemon/scripts/fe-audit.sh"
QUERY_PY="$ROOT/daemon/scripts/fe_audit_query.py"

if [ ! -x "$FE_AUDIT" ]; then
    echo "SKIP: $FE_AUDIT not executable; skipping smoke test"
    exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

LOG="$TMP/daemon.jsonl"
cat >"$LOG" <<EOF
{"timestamp":"2026-08-01T12:00:00Z","beadId":"jleechan-stuck-bead","lifecycleState":"ATTESTED","eventType":"STATE_TRANSITION","metrics":{},"context":{}}
{"timestamp":"2026-08-01T12:05:00Z","beadId":"jleechan-stuck-bead","lifecycleState":"DISPATCHED","eventType":"DISPATCH_REQUEST","metrics":{},"context":{}}
EOF

STATE_DIR="$TMP/fe-audit-state"
# Disable bead file via --no-bead (smoke test doesn't want to litter the br DB)
out="$(FE_AUDIT_LOG="$LOG" FE_AUDIT_STATE_DIR="$STATE_DIR" \
    REFILE_COOLDOWN_HOURS=0 \
    "$FE_AUDIT" --no-bead --log "$LOG" --lookback 24 2>&1 || true)"
echo "$out" | grep -q "G11: attested=0" || {
    echo "FAIL: G11 should report 0 stuck beads after DISPATCH_REQUEST pairing"
    echo "$out"
    exit 1
}
echo "OK: G11 reports 0 stuck beads after DISPATCH_REQUEST pairing"
