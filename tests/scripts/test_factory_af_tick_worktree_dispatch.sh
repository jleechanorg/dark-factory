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
git -C "$TARGET" branch fix/available

FAKE_R="$SCRATCH/remediate"
FAKE_R_LOG="$SCRATCH/remediate.log"
cat > "$FAKE_R" <<'EOF'
#!/usr/bin/env bash
printf 'bead=%s pr=%s sync=%s verify=%s\n' "$1" "$2" "${SYNC:-}" "${AFD_REQUIRE_SESSION:-}" >> "$AFD_TEST_REMEDIATE_LOG"
exit 0
EOF
chmod +x "$FAKE_R"
FAKE_I="$SCRATCH/intake"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_I"
chmod +x "$FAKE_I"
FAKE_DAEMON="$SCRATCH/daemon"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKE_DAEMON"
chmod +x "$FAKE_DAEMON"

export AFD_DB="$SCRATCH/overlay.sqlite"
export AFD_LOG="$SCRATCH/telemetry.jsonl"
export AFD_DAEMON_BIN="$FAKE_DAEMON"
export AFD_TEST_REMEDIATE_LOG="$FAKE_R_LOG"
cat > "$SCRATCH/daemon.toml" <<EOF
target_repo = "owner/target"
ao_project = "target"
max_workers = 30
max_batch = 15
[repos."owner/target"]
ao_project = "target"
local_checkout = "$TARGET"
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
run_tick available >/dev/null
available_state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='available';")"
assert "available branch becomes DISPATCHED after acknowledged remediation" "DISPATCHED" "$available_state"
assert "remediation asked for synchronous verification" "bead=available pr=744 sync=1 verify=1" "$(cat "$FAKE_R_LOG")"

echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
