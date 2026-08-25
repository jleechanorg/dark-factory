#!/usr/bin/env bash
# Atomic reservation contract for overlapping AF ticks.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"
SCRATCH="$(mktemp -d -t dispatch-reservation.XXXXXX)"
trap 'rm -rf "$SCRATCH"' EXIT
export AFD_DB="$SCRATCH/overlay.sqlite"
export AFD_LOG="$SCRATCH/telemetry.jsonl"
export CONFIG="$SCRATCH/daemon.toml"
cat > "$CONFIG" <<'EOF'
target_repo = "owner/repo"
ao_project = "repo"
max_workers = 30
max_batch = 15
EOF

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"; FAIL=$((FAIL + 1))
  fi
}

"$OVERLAY" init >/dev/null
"$OVERLAY" intake-upsert concurrent 'concurrent reservation' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=743, branch='fix/concurrent' WHERE bead_id='concurrent';"

set +e
"$OVERLAY" dispatch-reserve concurrent nonce-a >"$SCRATCH/a.out" 2>&1 & pid_a=$!
"$OVERLAY" dispatch-reserve concurrent nonce-b >"$SCRATCH/b.out" 2>&1 & pid_b=$!
wait "$pid_a"; rc_a=$?
wait "$pid_b"; rc_b=$?
set -e
successes=$(( (rc_a == 0 ? 1 : 0) + (rc_b == 0 ? 1 : 0) ))
refusals=$(( (rc_a == 10 ? 1 : 0) + (rc_b == 10 ? 1 : 0) ))
assert "overlapping reservations have exactly one winner" "1" "$successes"
assert "overlapping reservation loser gets EX_NOOP" "1" "$refusals"
winner="$(sqlite3 "$AFD_DB" "SELECT session_id FROM bead_overlay WHERE bead_id='concurrent';")"
assert "winner nonce is stored atomically" "yes" "$( [[ "$winner" = nonce-a || "$winner" = nonce-b ]] && echo yes || echo no )"
assert "reservation moves row to DISPATCHING" "DISPATCHING" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='concurrent';")"

ctx='{"reason":"test_release"}'
"$OVERLAY" dispatch-release concurrent "$winner" test_release "$ctx" >/dev/null
"$OVERLAY" dispatch-reserve concurrent nonce-new >/dev/null
set +e
"$OVERLAY" dispatch-complete concurrent fix/concurrent "$winner" >/dev/null 2>&1
old_rc=$?
set -e
assert "stale prior nonce cannot complete retry" "10" "$old_rc"
assert "stale completion leaves new reservation DISPATCHING" "DISPATCHING|nonce-new" "$(sqlite3 "$AFD_DB" "SELECT state || '|' || session_id FROM bead_overlay WHERE bead_id='concurrent';")"
"$OVERLAY" dispatch-complete concurrent fix/concurrent nonce-new >/dev/null
assert "current nonce completes DISPATCHED" "DISPATCHED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='concurrent';")"

echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
