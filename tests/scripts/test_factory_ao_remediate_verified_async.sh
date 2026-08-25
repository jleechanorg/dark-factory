#!/usr/bin/env bash
# The detached remediation path may report success only after the exact
# project-scoped PR session is visible. This exercises the production wrapper,
# not a mirrored dispatch loop.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REMEDIATE="$ROOT/daemon/factory-ao-remediate.sh"
SCRATCH="$(mktemp -d -t remediate-verified.XXXXXX)"
trap 'rm -rf "$SCRATCH"' EXIT

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"; FAIL=$((FAIL + 1))
  fi
}

FAKE_AO="$SCRATCH/ao-ts"
AO_LOG="$SCRATCH/ao.log"
cat > "$FAKE_AO" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$AFD_TEST_AO_LOG"
case "${1:-}" in
  spawn)
    if [ "${2:-}" = "--help" ]; then printf '%s\n' '--name'; exit 0; fi
    printf '%s\n' 'spawned session ao-verified'
    ;;
  session)
    if [ "${FAKE_VISIBLE_PR:-}" = "9002" ] && [ "$*" = 'session ls -p expected-project' ]; then
      printf '%s\n' 'ao-verified pulls/9002 [running]'
    fi
    ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$FAKE_AO"

wait_for_final() {
  local state_file="$1" state="" i
  for i in 1 2 3 4 5 6 7 8; do
    state="$(cat "$state_file" 2>/dev/null || true)"
    case "$state" in pending|'') sleep 1;; *) printf '%s' "$state"; return 0;; esac
  done
  printf '%s' "$state"
}

export AO_BIN="$FAKE_AO"
export AFD_TEST_AO_LOG="$AO_LOG"
export AFD_LOG_DIR="$SCRATCH/logs"
export AFD_SPAWN_STATE_DIR="$SCRATCH/states"

# Spawn output alone is insufficient: with no project-scoped session row the
# detached completion must become fail:* and remain ineligible for DISPATCHED.
AFD_REQUIRE_SESSION=1 AFD_ASYNC_WAIT_SEC=0 bash "$REMEDIATE" no-session 9001 owner/repo expected-project nonce-9001 >/dev/null
state="$(wait_for_final "$AFD_SPAWN_STATE_DIR/no-session-9001-nonce-9001.state")"
case "$state" in fail:*session_unverified) actual=fail;; *) actual="$state";; esac
assert "unverified async spawn writes failure state" "fail" "$actual"

# The same spawn succeeds only when session ls is scoped to the expected AO
# project and returns the exact PR.
export FAKE_VISIBLE_PR=9002
: > "$AO_LOG"
AFD_REQUIRE_SESSION=1 AFD_ASYNC_WAIT_SEC=0 bash "$REMEDIATE" visible-session 9002 owner/repo expected-project nonce-9002 >/dev/null
state="$(wait_for_final "$AFD_SPAWN_STATE_DIR/visible-session-9002-nonce-9002.state")"
assert "project-scoped visible session writes ok" "ok" "$state"
queries="$(grep -c '^session ls -p expected-project$' "$AO_LOG" || true)"
assert "verification query is project scoped" "yes" "$( [ "$queries" -ge 1 ] && echo yes || echo no )"

# Every readiness probe shares one wall-clock deadline. A CLI that hangs on
# both --version and status must not consume an unbounded multiple of it.
HANG_AO="$SCRATCH/hang-ao-ts"
cat > "$HANG_AO" <<'EOF'
#!/usr/bin/env bash
sleep 30
EOF
chmod +x "$HANG_AO"
start="$(date +%s)"
set +e
AFD_AO_READY_TIMEOUT_SEC=2 AO_BIN="$HANG_AO" AFD_REQUIRE_SESSION=1 AFD_ASYNC_WAIT_SEC=0 \
  timeout 6 bash "$REMEDIATE" hanging-probe 9003 owner/repo expected-project >/dev/null 2>&1
rc=$?
set -e
elapsed=$(( $(date +%s) - start ))
assert "hanging AO readiness fails instead of acknowledging spawn" "no" "$( [ "$rc" -eq 0 ] && echo yes || echo no )"
assert "all readiness probes stay within one deadline budget" "yes" "$( [ "$elapsed" -le 4 ] && echo yes || echo no )"

# Synchronous callers that require a verified session use the same readiness
# deadline. A successful spawn followed by a hanging session query must not
# escape the wrapper's bounded verification contract.
HANG_SESSION_AO="$SCRATCH/hang-session-ao-ts"
cat > "$HANG_SESSION_AO" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  'spawn --help') printf '%s\n' '--name' ;;
  spawn*) printf '%s\n' 'spawned session ao-sync' ;;
  'session ls -p expected-project') sleep 30 ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$HANG_SESSION_AO"
start="$(date +%s)"
set +e
SYNC=1 AFD_AO_READY_TIMEOUT_SEC=2 AO_BIN="$HANG_SESSION_AO" AFD_REQUIRE_SESSION=1 \
  timeout 6 bash "$REMEDIATE" hanging-sync-session 9004 owner/repo expected-project >/dev/null 2>&1
rc=$?
set -e
elapsed=$(( $(date +%s) - start ))
assert "hanging sync verification rejects dispatch" "1" "$rc"
assert "hanging sync verification stays within readiness deadline" "yes" "$( [ "$elapsed" -le 4 ] && echo yes || echo no )"

# The Go AO sync preflight status query is part of the same probe budget as
# spawn capability detection and required session verification.
HANG_STATUS_DIR="$SCRATCH/hang-status"
mkdir -p "$HANG_STATUS_DIR"
HANG_STATUS_AO="$HANG_STATUS_DIR/ao-go"
cat > "$HANG_STATUS_AO" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  'status --json') sleep 30 ;;
  daemon) exit 0 ;;
  'spawn --help') printf '%s\n' '--name' ;;
  spawn*) exit 1 ;;
  'session ls -p expected-project') sleep 30 ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$HANG_STATUS_AO"
start="$(date +%s)"
set +e
SYNC=1 AFD_AO_READY_TIMEOUT_SEC=2 AO_BIN="$HANG_STATUS_AO" AFD_REQUIRE_SESSION=1 \
  timeout 6 bash "$REMEDIATE" hanging-sync-status 9006 owner/repo expected-project >/dev/null 2>&1
rc=$?
set -e
elapsed=$(( $(date +%s) - start ))
assert "hanging sync status rejects dispatch" "1" "$rc"
assert "sync status shares readiness deadline" "yes" "$( [ "$elapsed" -le 4 ] && echo yes || echo no )"

# `ao spawn --help` is a capability probe, not the spawn itself, and must
# consume the same bounded probe budget as session verification.
HANG_HELP_AO="$SCRATCH/hang-help-ao-ts"
cat > "$HANG_HELP_AO" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  '--version'|'status') exit 0 ;;
  'spawn --help') sleep 30 ;;
  spawn*) exit 1 ;;
  'session ls -p expected-project') sleep 30 ;;
  *) exit 0 ;;
esac
EOF
chmod +x "$HANG_HELP_AO"
start="$(date +%s)"
AFD_AO_READY_TIMEOUT_SEC=2 AO_BIN="$HANG_HELP_AO" AFD_REQUIRE_SESSION=1 AFD_ASYNC_WAIT_SEC=0 \
  bash "$REMEDIATE" hanging-help 9005 owner/repo expected-project nonce-9005 >/dev/null 2>&1
state="$(wait_for_final "$AFD_SPAWN_STATE_DIR/hanging-help-9005-nonce-9005.state")"
elapsed=$(( $(date +%s) - start ))
case "$state" in fail:*) actual=fail;; *) actual="$state";; esac
assert "hanging spawn help records bounded failure" "fail" "$actual"
assert "spawn help and verification share one deadline" "yes" "$( [ "$elapsed" -le 4 ] && echo yes || echo no )"

echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
