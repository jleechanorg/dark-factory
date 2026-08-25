#!/usr/bin/env bash
# Regression for issue #743: an AO claim must not be recorded DISPATCHED
# before the target branch's worktree ownership is known and a verified spawn
# acknowledgement has been requested.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"
TICK="$ROOT/daemon/factory-af-tick.sh"

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"; FAIL=$((FAIL + 1))
  fi
}
assert_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file" 2>/dev/null; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (pattern '$pattern' not found in $file)"; FAIL=$((FAIL + 1))
  fi
}

SCRATCH="$(mktemp -d -t af-worktree-dispatch.XXXXXX)"
trap 'rm -rf "$SCRATCH"' EXIT
TARGET="$SCRATCH/target"
git init -q -b main "$TARGET"
git -C "$TARGET" config user.email test@example.invalid
git -C "$TARGET" config user.name test
touch "$TARGET/.keep"
git -C "$TARGET" add .keep
git -C "$TARGET" commit -qm initial
git -C "$TARGET" remote add origin https://github.com/owner/target.git
git -C "$TARGET" branch fix/available

MANAGED_ROOT="$SCRATCH/managed"
mkdir -p "$MANAGED_ROOT/owner"
mv "$TARGET" "$MANAGED_ROOT/owner/target"
TARGET="$MANAGED_ROOT/owner/target"

FAKE_R="$SCRATCH/remediate"
FAKE_R_LOG="$SCRATCH/remediate.log"
cat > "$FAKE_R" <<'EOF'
#!/usr/bin/env bash
printf 'bead=%s pr=%s async=%s sync=%s verify=%s\n' "$1" "$2" "${ASYNC:-}" "${SYNC:-}" "${AFD_REQUIRE_SESSION:-}" >> "$AFD_TEST_REMEDIATE_LOG"
if [ "${SYNC:-0}" = "1" ]; then sleep 3; fi
mkdir -p "$AFD_SPAWN_STATE_DIR"
printf 'pending\n' > "$AFD_SPAWN_STATE_DIR/$1-$2.state"
exit 0
EOF
chmod +x "$FAKE_R"
FAKE_I="$SCRATCH/intake"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_I"
chmod +x "$FAKE_I"
FAKE_DAEMON="$SCRATCH/daemon"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_DAEMON"
chmod +x "$FAKE_DAEMON"

FAKE_AO="$SCRATCH/ao-ts"
cat > "$FAKE_AO" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  session)
    if [ -n "${AFD_TEST_ACTIVE_PR:-}" ]; then
      printf 'ao-test pulls/%s [running]\n' "$AFD_TEST_ACTIVE_PR"
    fi
    ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$FAKE_AO"

export AFD_DB="$SCRATCH/overlay.sqlite"
export AFD_LOG="$SCRATCH/telemetry.jsonl"
export AFD_DAEMON_BIN="$FAKE_DAEMON"
export AFD_TEST_REMEDIATE_LOG="$FAKE_R_LOG"
export AFD_SPAWN_STATE_DIR="$SCRATCH/spawns"
export DARK_FACTORY_TARGET_WORKTREE_ROOT="$MANAGED_ROOT"
export AO_BIN="$FAKE_AO"
cat > "$SCRATCH/daemon.toml" <<EOF
target_repo = "owner/target"
ao_project = "target"
max_workers = 30
max_batch = 15
[repos."owner/target"]
ao_project = "target"
EOF
export CONFIG="$SCRATCH/daemon.toml"
"$OVERLAY" init >/dev/null

run_tick() {
  AFD_REMEDIATE_BIN="$FAKE_R" AFD_INTAKE_BIN="$FAKE_I" \
  AFD_SKIP_DRIFT_CHECK=1 AFD_BEAD_FILTER="$1" bash "$TICK" 2>&1
}

# An operator's checkout of the exact PR branch is not an AO-reusable
# workspace. The tick must leave the bead queued and emit a structured reason.
git -C "$TARGET" checkout -q -b fix/occupied
"$OVERLAY" intake-upsert occupied 'occupied branch' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=743, branch='fix/occupied', target_repo='owner/target' WHERE bead_id='occupied';"
: > "$FAKE_R_LOG"
occupied_out="$(run_tick occupied)"
occupied_state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='occupied';")"
assert "occupied branch remains QUEUED" "QUEUED" "$occupied_state"
assert "occupied branch does not call remediation" "0" "$(wc -l < "$FAKE_R_LOG" | tr -d ' ')"
assert_grep "occupied branch reports structured block" '"eventType": "TASK_DISPATCH_BLOCKED".*"reason": "branch_checked_out"' "$AFD_LOG"
case "$occupied_out" in
  *branch_checked_out*) echo "PASS: occupied branch prints blocked reason"; PASS=$((PASS + 1));;
  *) echo "FAIL: occupied branch output missing blocked reason: $occupied_out"; FAIL=$((FAIL + 1));;
esac

git -C "$TARGET" checkout -q main
"$OVERLAY" intake-upsert available 'available branch' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=744, branch='fix/available', target_repo='owner/target' WHERE bead_id='available';"
: > "$FAKE_R_LOG"
mkdir -p "$AFD_SPAWN_STATE_DIR"
printf 'ok\n' > "$AFD_SPAWN_STATE_DIR/available-744.state"
export AFD_TEST_ACTIVE_PR=744
git -C "$TARGET" checkout -q fix/available
start="$(date +%s)"
run_tick available >/dev/null
elapsed=$(( $(date +%s) - start ))
git -C "$TARGET" checkout -q main
unset AFD_TEST_ACTIVE_PR
available_state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='available';")"
assert "compatible active session reuses its occupied worktree after reconciliation" "DISPATCHED" "$available_state"
assert "verified existing session avoids duplicate remediation" "0" "$(wc -l < "$FAKE_R_LOG" | tr -d ' ')"
assert "verified reconciliation completes without spawn-time blocking" "yes" "$( [ "$elapsed" -lt 2 ] && echo yes || echo no )"

# A fresh spawn is detached and retained as QUEUED while pending. The fake
# remediation deliberately sleeps in SYNC mode, so this latency check catches
# any regression that puts spawn timeout work back into the tick.
git -C "$TARGET" branch fix/pending
"$OVERLAY" intake-upsert pending 'pending async spawn' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=745, branch='fix/pending', target_repo='owner/target' WHERE bead_id='pending';"
: > "$FAKE_R_LOG"
start="$(date +%s)"
run_tick pending >/dev/null
elapsed=$(( $(date +%s) - start ))
pending_state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='pending';")"
assert "pending async spawn remains QUEUED" "QUEUED" "$pending_state"
assert "tick requests async verified remediation" "bead=pending pr=745 async=1 sync= verify=1" "$(cat "$FAKE_R_LOG")"
assert "async dispatch tick avoids overlap latency" "yes" "$( [ "$elapsed" -lt 2 ] && echo yes || echo no )"

# An `ok` state without a visible session is allowed a bounded visibility
# window, then becomes a structured retry instead of wedging QUEUED forever.
git -C "$TARGET" branch fix/orphaned
"$OVERLAY" intake-upsert orphaned 'orphaned verified state' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=748, branch='fix/orphaned', target_repo='owner/target' WHERE bead_id='orphaned';"
printf 'ok\n' > "$AFD_SPAWN_STATE_DIR/orphaned-748.state"
touch -d '10 minutes ago' "$AFD_SPAWN_STATE_DIR/orphaned-748.state"
: > "$FAKE_R_LOG"
AFD_PENDING_MAX_AGE_SEC=1 run_tick orphaned >/dev/null
assert "stale verified state remains QUEUED while retrying" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='orphaned';")"
assert "stale verified state starts a new async attempt" "bead=orphaned pr=748 async=1 sync= verify=1" "$(cat "$FAKE_R_LOG")"
assert_grep "stale verified state emits JSON blocked reason" '"eventType": "TASK_DISPATCH_BLOCKED".*"reason": "verified_session_missing"' "$AFD_LOG"

# Missing and unmapped repo routing are retryable dispatch blocks, never AO
# calls or HUMAN_HELD transitions, and carry machine-readable reasons.
"$OVERLAY" intake-upsert missing-repo 'missing repo' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=746, branch='fix/missing-repo', target_repo=NULL WHERE bead_id='missing-repo';"
: > "$FAKE_R_LOG"
run_tick missing-repo >/dev/null
assert "missing repo remains QUEUED" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='missing-repo';")"
assert "missing repo does not call AO" "0" "$(wc -l < "$FAKE_R_LOG" | tr -d ' ')"
assert_grep "missing repo emits JSON blocked reason" '"eventType": "TASK_DISPATCH_BLOCKED".*"reason": "missing_target_repo"' "$AFD_LOG"

"$OVERLAY" intake-upsert unmapped-repo 'unmapped repo' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=747, branch='fix/unmapped-repo', target_repo='owner/unknown' WHERE bead_id='unmapped-repo';"
: > "$FAKE_R_LOG"
run_tick unmapped-repo >/dev/null
assert "unmapped repo remains QUEUED" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='unmapped-repo';")"
assert "unmapped repo does not call AO" "0" "$(wc -l < "$FAKE_R_LOG" | tr -d ' ')"
assert_grep "unmapped repo emits JSON blocked reason" '"eventType": "TASK_DISPATCH_BLOCKED".*"reason": "unmapped_target_repo"' "$AFD_LOG"

echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
