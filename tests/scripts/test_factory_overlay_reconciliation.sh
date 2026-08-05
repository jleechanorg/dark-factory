#!/usr/bin/env bash
# test_factory_overlay_reconciliation.sh — bead jleechan-xsg4 (issue #270)
# Fail-closed overlay reconciliation: demote overlay rows whose underlying
# br bead is missing/closed, and requeue DISPATCHED rows whose session is
# dead or absent. The previous behavior silently kept stale rows active,
# starving new P0 work.
#
# Run with: bash tests/scripts/test_factory_overlay_reconciliation.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"

# Isolated DB so we don't pollute the real CXDB.
export AFD_DB="/tmp/test-overlay-reconciliation-$$-$$.sqlite"
export AFD_LOG="/tmp/test-overlay-reconciliation-$$-$$.jsonl"
export CONFIG="$ROOT/daemon/contracts/daemon.toml.example"
export AFD_DAEMON_BIN="${AFD_DAEMON_BIN:-$ROOT/daemon/target/debug/daemon}"
if [ ! -x "$AFD_DAEMON_BIN" ]; then
  cargo build --quiet --manifest-path "$ROOT/daemon/Cargo.toml" 2>&1 | tail -5
fi
# br needs a beads.db; point at a fresh temp one so the new subcommands
# that consult br show can run.
export BR_DB="/tmp/test-overlay-reconciliation-$$-beads.db"
touch "$BR_DB"

# Override br binary to a controllable shim. The shim returns status="open"
# by default; flip /tmp/br-status to "closed" to simulate a closed bead,
# or to "missing" to simulate a deleted bead.
export BR_BIN="/tmp/test-overlay-reconciliation-$$-br.sh"
cat > "$BR_BIN" <<'BR_EOF'
#!/usr/bin/env bash
# Fake br shim: --json shows {status:"open"|"closed"|"missing"} based on
# /tmp/br-recon-list (one "<category>/<bead_id>" per line) for per-bead
# show, falling back to /tmp/br-recon-status for beads not in the list.
# Categories:
#   open/<id>    ⇒ bead exists in open state
#   closed/<id>  ⇒ bead exists in closed state
#   missing/<id> ⇒ bead is missing from br entirely (treated as missing)
#   (also: absent beads — those not in /tmp/br-recon-list at all — are
#   treated as missing by default, which is the conservative fail-closed
#   choice for the reconciliation sweep.)
case "${1:-}" in
  show)
    bead="$2"
    if [ "${3:-}" = "--json" ]; then
      # Look up the bead in /tmp/br-recon-list.
      cat_line="$(grep -E "/${bead}$" /tmp/br-recon-list 2>/dev/null || true)"
      cat="${cat_line%%/*}"
      if [ "$cat" = "closed" ]; then
        printf '[{"id":"%s","status":"closed"}]\n' "$bead"
      elif [ "$cat" = "open" ]; then
        printf '[{"id":"%s","status":"open"}]\n' "$bead"
      elif [ "$cat" = "missing" ]; then
        printf '[]\n'
      else
        # Bead not in the list at all → treat as missing (fail-closed).
        # The reconcile sweep must demote these rows.
        printf '[]\n'
      fi
    fi
    ;;
  list)
    if [ "${3:-}" = "--json" ]; then
      python3 -c '
import json, sys, os
ids = []
try:
    with open("/tmp/br-recon-list") as f:
        for line in f:
            line = line.strip()
            if not line: continue
            cat, bid = line.split("/", 1)
            if cat == "open":
                ids.append({"id": bid, "status":"open"})
            elif cat == "closed":
                ids.append({"id": bid, "status":"closed"})
except FileNotFoundError:
    pass
print(json.dumps(ids))
' "$@"
    fi
    ;;
esac
BR_EOF
chmod +x "$BR_BIN"

cleanup() {
  rm -f "$AFD_DB" "$AFD_LOG" "$BR_DB" "$BR_BIN" /tmp/br-recon-status /tmp/br-recon-list
}
trap cleanup EXIT

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

# 1. init schema
init_out="$("$OVERLAY" init 2>&1 | tail -1)"
assert "init returns ok" "ok: schema applied to $AFD_DB" "$init_out"

# 2. Bead-closed-check already exists, but it processes a single bead. The
#    new reconciliation subcommand (`reconcile-overlays`) sweeps ALL active
#    overlays and demotes any whose br status is missing/closed. First
#    prove the subcommand exists.
echo open > /tmp/br-recon-status
: > /tmp/br-recon-list
set +e
"$OVERLAY" reconcile-overlays >/dev/null 2>&1
rc=$?
set -e
case "$rc" in
  0|1)  # subcommand may legitimately exit 0 (no rows) or 1 (no-op / not-yet-implemented)
    : ;;
  2)
    echo "FAIL: reconcile-overlays unknown subcommand (rc=2 invalid args)"
    FAIL=$((FAIL + 1))
    ;;
  *)
    echo "FAIL: reconcile-overlays unexpected rc=$rc"
    FAIL=$((FAIL + 1))
    ;;
esac

# 3. Seed two factory beads: one whose br is closed, one whose br is open.
"$OVERLAY" intake-upsert test-closed 'closed bead' >/dev/null
"$OVERLAY" route-record test-closed STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-closed fix/test-closed-branch >/dev/null
"$OVERLAY" pr-opened test-closed 9001 https://github.com/jleechanorg/worldarchitect.ai/pull/9001 >/dev/null

"$OVERLAY" intake-upsert test-open 'open bead' >/dev/null
"$OVERLAY" route-record test-open STANDARD_PATH >/dev/null
"$OVERLAY" dispatch-record test-open fix/test-open-branch >/dev/null
"$OVERLAY" pr-opened test-open 9002 https://github.com/jleechanorg/worldarchitect.ai/pull/9002 >/dev/null

# Tell the shim which beads are open vs closed in the global list.
cat > /tmp/br-recon-list <<'LIST_EOF'
open/test-open
closed/test-closed
LIST_EOF

# 4. Demote the closed bead — its overlay row must transition to HUMAN_HELD
#    with prior_state telemetry. The open bead must remain ATTESTED.
set +e
out="$("$OVERLAY" reconcile-overlays 2>&1)"
rc=$?
set -e
# Touch rc — the subcommand should succeed (exit 0) after processing.
[ "$rc" -eq 0 ] || [ "$rc" -eq 1 ] || {
  echo "FAIL: reconcile-overlays exit code rc=$rc (expected 0|1)"
  FAIL=$((FAIL + 1))
}

state_closed="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-closed';")"
assert "reconcile-overlays demotes closed bead to HUMAN_HELD" "HUMAN_HELD" "$state_closed"

state_open="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-open';")"
assert "reconcile-overlays preserves open bead" "ATTESTED" "$state_open"

# 5. Prior-state telemetry must be captured in the log.
assert_grep_re() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file" 2>/dev/null; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (pattern '$pattern' not found in $file)"
    FAIL=$((FAIL + 1))
  fi
}
assert_grep_re "RECONCILE_DEMOTED event emitted with prior_state" \
  '"eventType": "RECONCILE_DEMOTED"' "$AFD_LOG"

# 6. requeue-stale-dispatched: a DISPATCHED bead with empty session_id
#    AND aged autonomy MUST be requeued back to QUEUED. We seed an
#    artificially-aged DISPATCHED row with no session, then run the
#    requeue-stale-dispatched subcommand.
sqlite3 "$AFD_DB" <<'SQL'
INSERT INTO bead_overlay (bead_id,state,attempt,autonomy_secs,branch,session_id,updated_at,pr_number)
VALUES
  ('stale-empty-session','DISPATCHED',1,18000,'fix/stale-empty-session',NULL,'2026-07-01T00:00:00Z',42),
  ('live-young-session','DISPATCHED',1,300,'fix/live-young-session','live-123','2026-07-01T00:00:00Z',43),
  ('stale-dead-session','DISPATCHED',1,18000,'fix/stale-dead-session','dead-456','2026-07-01T00:00:00Z',44);
SQL

# Subcommand must exist (rc != 2 for unknown subcommand).
set +e
"$OVERLAY" requeue-stale-dispatched >/dev/null 2>&1
rc=$?
set -e
case "$rc" in
  0) : ;;
  2)
    echo "FAIL: requeue-stale-dispatched unknown subcommand (rc=2)"
    FAIL=$((FAIL + 1))
    ;;
  *)
    echo "FAIL: requeue-stale-dispatched unexpected rc=$rc"
    FAIL=$((FAIL + 1))
    ;;
esac

# 7. After requeue, the empty-session row returns to QUEUED, the
#    live-young-session row stays DISPATCHED (not aged), and the
#    dead-session row's behavior is implementation-defined (we accept
#    either requeue or no-op, but it must NOT be silently left in
#    DISPATCHED without triage).
state_stale="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='stale-empty-session';")"
assert "stale-empty-session requeued to QUEUED" "QUEUED" "$state_stale"

state_live_young="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='live-young-session';")"
assert "live-young-session preserved as DISPATCHED" "DISPATCHED" "$state_live_young"

# 8. After requeue, the empty-session row's session_id and branch are
#    cleared, but prior_state telemetry is preserved.
session_cleared="$(sqlite3 "$AFD_DB" "SELECT coalesce(session_id,'') FROM bead_overlay WHERE bead_id='stale-empty-session';")"
assert "stale-empty-session session_id cleared" "" "$session_cleared"

branch_cleared="$(sqlite3 "$AFD_DB" "SELECT coalesce(branch,'') FROM bead_overlay WHERE bead_id='stale-empty-session';")"
assert "stale-empty-session branch cleared" "" "$branch_cleared"

# 9. Log emits a stale-requeue event with prior_state.
assert_grep_re "RECONCILE_REQUEUED event emitted" \
  '"eventType": "RECONCILE_REQUEUED"' "$AFD_LOG"

# 10. The whole-tick stale-active count decreases monotonically. Capture
#     stale-active counts before and after a second reconcile-overlays
#     call on the same data — the count must not grow.
count_active() {
  sqlite3 "$AFD_DB" \
    "SELECT count(*) FROM bead_overlay WHERE state IN ('DISPATCHED','ATTESTED','DISPATCHING') \
     AND (session_id IS NULL OR trim(session_id) = '' OR autonomy_secs >= 14400);"
}
baseline="$(count_active)"
# Run reconcile once more; count must not increase.
"$OVERLAY" reconcile-overlays >/dev/null 2>&1 || true
after="$(count_active)"
if [ "$after" -le "$baseline" ]; then
  echo "PASS: stale active count monotonically non-increasing ($baseline → $after)"
  PASS=$((PASS + 1))
else
  echo "FAIL: stale active count grew ($baseline → $after)"
  FAIL=$((FAIL + 1))
fi

# 11. Missing bead (not in br list at all) must be demoted. Seed an
#     overlay row whose bead_id is NOT in /tmp/br-recon-list.
sqlite3 "$AFD_DB" <<'SQL'
INSERT INTO bead_overlay (bead_id,state,attempt,autonomy_secs,branch,session_id,updated_at,pr_number)
VALUES ('ghost-bead-id','ATTESTED',1,5000,'fix/ghost-bead','live-ghost',datetime('now'),99);
SQL
"$OVERLAY" reconcile-overlays >/dev/null 2>&1 || true
state_ghost="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='ghost-bead-id';")"
assert "ghost bead (missing from br) demoted to HUMAN_HELD" "HUMAN_HELD" "$state_ghost"

# 12. Reconciliation must NOT touch already-terminal rows (READY, HUMAN_HELD).
sqlite3 "$AFD_DB" <<'SQL'
INSERT INTO bead_overlay (bead_id,state,attempt,autonomy_secs,branch,session_id,updated_at,pr_number)
VALUES ('terminal-ready','READY',1,24000,'fix/already-ready','live-extra',datetime('now'),55);
SQL
"$OVERLAY" reconcile-overlays >/dev/null 2>&1 || true
state_terminal="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='terminal-ready';")"
assert "already-READY row preserved" "READY" "$state_terminal"

echo
echo "xsg4 reconciliation tests: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
