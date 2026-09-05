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
"$OVERLAY" dispatch-release concurrent "$winner" stale_release "$ctx" >/dev/null 2>&1
stale_release_rc=$?
"$OVERLAY" dispatch-complete concurrent fix/concurrent "$winner" >/dev/null 2>&1
old_rc=$?
set -e
assert "stale prior nonce cannot release retry" "10" "$stale_release_rc"
assert "stale release leaves new reservation DISPATCHING" "DISPATCHING|nonce-new" "$(sqlite3 "$AFD_DB" "SELECT state || '|' || session_id FROM bead_overlay WHERE bead_id='concurrent';")"
assert "stale prior nonce cannot complete retry" "10" "$old_rc"
assert "stale completion leaves new reservation DISPATCHING" "DISPATCHING|nonce-new" "$(sqlite3 "$AFD_DB" "SELECT state || '|' || session_id FROM bead_overlay WHERE bead_id='concurrent';")"
"$OVERLAY" dispatch-complete concurrent fix/concurrent nonce-new >/dev/null
assert "current nonce completes DISPATCHED" "DISPATCHED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='concurrent';")"

# Capacity admission belongs to the reservation transaction. Two processes
# racing for the final batch slot must not both enter DISPATCHING.
cat > "$CONFIG" <<'EOF'
target_repo = "owner/repo"
ao_project = "repo"
max_workers = 30
max_batch = 1
EOF
for bead in cap-a cap-b; do
  "$OVERLAY" intake-upsert "$bead" 'capacity reservation' >/dev/null
  sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=744, branch='fix/$bead' WHERE bead_id='$bead';"
done
set +e
"$OVERLAY" dispatch-reserve cap-a nonce-cap-a >"$SCRATCH/cap-a.out" 2>&1 & cap_pid_a=$!
"$OVERLAY" dispatch-reserve cap-b nonce-cap-b >"$SCRATCH/cap-b.out" 2>&1 & cap_pid_b=$!
wait "$cap_pid_a"; cap_rc_a=$?
wait "$cap_pid_b"; cap_rc_b=$?
set -e
cap_successes=$(( (cap_rc_a == 0 ? 1 : 0) + (cap_rc_b == 0 ? 1 : 0) ))
cap_refusals=$(( (cap_rc_a == 3 ? 1 : 0) + (cap_rc_b == 3 ? 1 : 0) ))
assert "concurrent final batch slot has one winner" "1" "$cap_successes"
assert "concurrent capacity loser gets EX_OVER_CAP" "1" "$cap_refusals"
assert "max_batch counts active DISPATCHING reservations" "0" "$("$OVERLAY" capacity)"

# The worker cap also counts in-flight reservations, not only already
# DISPATCHED/ATTESTED rows.
cat > "$CONFIG" <<'EOF'
target_repo = "owner/repo"
ao_project = "repo"
max_workers = 2
max_batch = 15
EOF
"$OVERLAY" intake-upsert cap-worker 'worker capacity reservation' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=745, branch='fix/cap-worker' WHERE bead_id='cap-worker';"
set +e
"$OVERLAY" dispatch-reserve cap-worker nonce-cap-worker >/dev/null 2>&1
worker_cap_rc=$?
set -e
assert "DISPATCHING reservation consumes worker capacity" "3" "$worker_cap_rc"
assert "worker-cap refusal leaves bead QUEUED" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='cap-worker';")"

echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
