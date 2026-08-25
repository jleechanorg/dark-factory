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
[ "${AFD_TEST_REMEDIATE_SLEEP:-0}" = "0" ] || sleep "$AFD_TEST_REMEDIATE_SLEEP"
mkdir -p "$AFD_SPAWN_STATE_DIR"
printf 'pending\n' > "$AFD_SPAWN_STATE_DIR/$1-$2-${5:-legacy}.state"
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
if [ "${AFD_TEST_AO_HANG:-0}" = "1" ]; then sleep 30; fi
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
pending_nonce="$(sqlite3 "$AFD_DB" "SELECT session_id FROM bead_overlay WHERE bead_id='pending';")"
assert "pending async spawn remains atomically reserved" "DISPATCHING" "$pending_state"
assert "tick requests async verified remediation" "bead=pending pr=745 async=1 sync= verify=1" "$(cut -d' ' -f1-5 "$FAKE_R_LOG")"
assert "pending state file is scoped to reservation nonce" "yes" "$( [ -f "$AFD_SPAWN_STATE_DIR/pending-745-$pending_nonce.state" ] && echo yes || echo no )"
assert "async dispatch tick avoids overlap latency" "yes" "$( [ "$elapsed" -lt 2 ] && echo yes || echo no )"
printf 'ok\n' > "$AFD_SPAWN_STATE_DIR/pending-745-$pending_nonce.state"
export AFD_TEST_ACTIVE_PR=745
run_tick pending >/dev/null
unset AFD_TEST_ACTIVE_PR
assert "matching nonce plus exact active session reconciles DISPATCHED" "DISPATCHED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='pending';")"

# A fresh compatible session can still lose branch registration at completion.
# The tick must not die under set -e or leave its reservation wedged.
git -C "$TARGET" branch fix/fresh-complete-conflict
"$OVERLAY" intake-upsert fresh-complete-conflict 'fresh completion conflict' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=753, branch='fix/fresh-complete-conflict', target_repo='owner/target' WHERE bead_id='fresh-complete-conflict'; INSERT INTO branch_registry(branch,bead_id,created_at) VALUES('fix/fresh-complete-conflict','other-owner',datetime('now'));"
export AFD_TEST_ACTIVE_PR=753
set +e
fresh_complete_out="$(run_tick fresh-complete-conflict)"
fresh_complete_rc=$?
set -e
unset AFD_TEST_ACTIVE_PR
assert "fresh completion conflict does not crash tick" "0" "$fresh_complete_rc"
assert "fresh completion conflict releases reservation" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='fresh-complete-conflict';")"
assert_grep "fresh completion conflict emits structured block" '"beadId": "fresh-complete-conflict".*"state": "QUEUED".*"reason": "dispatch_complete_failed"' "$AFD_LOG"

# The detached reconciliation path has the same completion failure contract.
git -C "$TARGET" branch fix/reconcile-complete-conflict
"$OVERLAY" intake-upsert reconcile-complete-conflict 'reconciliation completion conflict' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=754, branch='fix/reconcile-complete-conflict', target_repo='owner/target' WHERE bead_id='reconcile-complete-conflict'; INSERT INTO branch_registry(branch,bead_id,created_at) VALUES('fix/reconcile-complete-conflict','other-owner',datetime('now'));"
"$OVERLAY" dispatch-reserve reconcile-complete-conflict nonce-reconcile-conflict >/dev/null
printf 'ok\n' > "$AFD_SPAWN_STATE_DIR/reconcile-complete-conflict-754-nonce-reconcile-conflict.state"
export AFD_TEST_ACTIVE_PR=754
run_tick reconcile-complete-conflict >/dev/null
unset AFD_TEST_ACTIVE_PR
assert "reconciliation completion conflict releases reservation" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='reconcile-complete-conflict';")"
assert_grep "reconciliation completion conflict emits structured block" '"beadId": "reconcile-complete-conflict".*"state": "QUEUED".*"reason": "dispatch_complete_failed"' "$AFD_LOG"

# An `ok` state without a visible session is allowed a bounded visibility
# window, then becomes a structured retry instead of wedging QUEUED forever.
git -C "$TARGET" branch fix/orphaned
"$OVERLAY" intake-upsert orphaned 'orphaned verified state' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=748, branch='fix/orphaned', target_repo='owner/target' WHERE bead_id='orphaned';"
"$OVERLAY" dispatch-reserve orphaned nonce-orphaned >/dev/null
printf 'ok\n' > "$AFD_SPAWN_STATE_DIR/orphaned-748-nonce-orphaned.state"
touch -d '10 minutes ago' "$AFD_SPAWN_STATE_DIR/orphaned-748-nonce-orphaned.state"
: > "$FAKE_R_LOG"
AFD_PENDING_MAX_AGE_SEC=1 run_tick orphaned >/dev/null
assert "stale verified state remains QUEUED while retrying" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='orphaned';")"
assert "stale verified state does not spawn until a new tick reserves" "0" "$(wc -l < "$FAKE_R_LOG" | tr -d ' ')"
assert_grep "stale verified state emits JSON blocked reason" '"eventType": "TASK_DISPATCH_BLOCKED".*"reason": "verified_session_missing"' "$AFD_LOG"

# Two real overlapping tick processes race on one QUEUED bead. The database
# reservation must admit one wrapper call, and the attempt nonce must be the
# one persisted in both the row and state filename.
git -C "$TARGET" branch fix/concurrent-tick
"$OVERLAY" intake-upsert concurrent-tick 'overlapping tick reservation' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=751, branch='fix/concurrent-tick', target_repo='owner/target' WHERE bead_id='concurrent-tick';"
: > "$FAKE_R_LOG"
export AFD_TEST_REMEDIATE_SLEEP=1
run_tick concurrent-tick >/dev/null & tick_a=$!
run_tick concurrent-tick >/dev/null & tick_b=$!
wait "$tick_a"
wait "$tick_b"
unset AFD_TEST_REMEDIATE_SLEEP
assert "overlapping ticks invoke AO claim once" "1" "$(wc -l < "$FAKE_R_LOG" | tr -d ' ')"
concurrent_row="$(sqlite3 "$AFD_DB" "SELECT state || '|' || coalesce(session_id,'') FROM bead_overlay WHERE bead_id='concurrent-tick';")"
case "$concurrent_row" in DISPATCHING'|'?*) actual=yes;; *) actual=no;; esac
assert "overlapping tick winner persists reservation nonce" "yes" "$actual"

# The real tick also bounds its AO inventory/session probes; otherwise a
# hanging CLI prevents it from ever reaching the bounded remediation wrapper.
git -C "$TARGET" branch fix/hanging-ao
"$OVERLAY" intake-upsert hanging-ao 'hanging AO probe' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=752, branch='fix/hanging-ao', target_repo='owner/target' WHERE bead_id='hanging-ao';"
start="$(date +%s)"
set +e
AFD_TEST_AO_HANG=1 AFD_AO_READY_TIMEOUT_SEC=1 AFD_REMEDIATE_BIN="$FAKE_R" \
  AFD_INTAKE_BIN="$FAKE_I" AFD_SKIP_DRIFT_CHECK=1 AFD_BEAD_FILTER=hanging-ao \
  timeout 6 bash "$TICK" >/dev/null 2>&1
hang_rc=$?
set -e
elapsed=$(( $(date +%s) - start ))
assert "hanging AO tick completes within readiness budget" "yes" "$( [ "$hang_rc" -eq 0 ] && [ "$elapsed" -le 5 ] && echo yes || echo no )"

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

# URL suffix tricks must not pass origin validation. Only github.com with the
# exact /owner/repo path is accepted.
for case_name in evil-host attacker-host path-lookalike; do
  case "$case_name" in
    evil-host) bad_url='https://evilgithub.com/owner/target.git' ;;
    attacker-host) bad_url='https://github.com.attacker.example/owner/target.git' ;;
    path-lookalike) bad_url='https://github.com/owner/target-extra.git' ;;
  esac
  git -C "$TARGET" remote set-url origin "$bad_url"
  bead="url-$case_name"
  "$OVERLAY" intake-upsert "$bead" 'bad origin URL' >/dev/null
  sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=749, branch='fix/url-$case_name', target_repo='owner/target' WHERE bead_id='$bead';"
  : > "$FAKE_R_LOG"
  run_tick "$bead" >/dev/null
  assert "$case_name origin remains QUEUED" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='$bead';")"
  assert "$case_name origin never calls AO" "0" "$(wc -l < "$FAKE_R_LOG" | tr -d ' ')"
done
git -C "$TARGET" remote set-url origin https://github.com/owner/target.git

# A blocked ATTESTED row must be atomically requeued, not left in a state that
# dispatch-blocked merely reports without changing.
"$OVERLAY" intake-upsert attested-block 'attested blocked row' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET state='ATTESTED', pr_number=750, branch='fix/attested-block', target_repo=NULL WHERE bead_id='attested-block';"
: > "$FAKE_R_LOG"
run_tick attested-block >/dev/null
assert "blocked ATTESTED row transitions to QUEUED" "QUEUED" "$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='attested-block';")"
assert "blocked ATTESTED row never calls AO" "0" "$(wc -l < "$FAKE_R_LOG" | tr -d ' ')"
assert_grep "blocked ATTESTED telemetry records QUEUED" '"beadId": "attested-block".*"state": "QUEUED".*"eventType": "TASK_DISPATCH_BLOCKED"' "$AFD_LOG"

echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
