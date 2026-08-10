#!/usr/bin/env bash
# test_killmode_process_boundary.sh — REAL process-boundary proof for the
# daemon's KillMode= systemd setting.
#
# The PR that introduced KillMode=process to
# daemon/systemd/ai.dark-factory.daemon.service.template was verified by
# string-grep tests only (tests/scripts/test_systemd_user_install.sh,
# tests/test_systemd_user_install.py) — neither one spawns a real process
# tree or observes actual kill/survival semantics. This script closes that
# gap with a live systemd --user transient-unit harness:
#
#   GREEN (production config): spawn a service whose main process forks a
#   detached long-running child (simulating a live AGY/AO worker session in
#   the same cgroup), `systemctl --user restart` the service under the
#   daemon's ACTUAL rendered KillMode/KillSignal (read from the real
#   template via install-systemd-user.sh --render-only, not hand-picked),
#   and assert the child process SURVIVES the restart (kill -0). Then
#   simulate the ao Sessions::stop path (CliSessions::stop ->
#   `ao session kill <id>`, daemon/src/adapters.rs:4349), which kills the
#   worker PID DIRECTLY and independently of systemd — assert that direct
#   kill succeeds and the child is fully reaped, i.e. no orphan remains.
#
#   RED-proof (discrimination control): repeat the identical harness with
#   KillMode defaulted to systemd's standard `control-group` (the mode this
#   PR replaced) and assert the child DIES on restart. This proves the
#   harness actually discriminates KillMode behavior rather than always
#   passing regardless of config — if the RED case did NOT show the child
#   dying, the GREEN case's "survival" assertion would be meaningless.
#
# SKIPS LOUDLY (exit 0, warning to stderr) when no real systemd --user
# manager is available (GitHub-hosted CI, containers without systemd/D-Bus,
# macOS dev hosts) so it never false-fails CI on hosts that structurally
# cannot run it. Only self-hosted Linux runners with a live systemd --user
# session (e.g. jeff-ubuntu) execute the real assertions.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
INSTALLER="$ROOT/daemon/systemd/install-systemd-user.sh"

# --- Probe: is a real systemd --user manager available on this host? ---
if ! systemctl --user show-environment >/dev/null 2>&1; then
  echo "SKIP: no systemd --user manager available on this host; KillMode process-boundary test requires real systemd --user (e.g. jeff-ubuntu self-hosted runner). Skipping loudly, not false-failing CI." >&2
  exit 0
fi
if ! command -v systemd-run >/dev/null 2>&1; then
  echo "SKIP: systemd-run not found on PATH; cannot spawn transient test units. Skipping loudly, not false-failing CI." >&2
  exit 0
fi

PASS=0
FAIL=0

TMP="$(mktemp -d -t dark-factory-killmode-test.XXXXXX)"
SUFFIX="$(basename "$TMP")"
UNIT_GREEN="df-killmode-test-green-${SUFFIX}"
UNIT_RED="df-killmode-test-red-${SUFFIX}"

cleanup() {
  local ec=$?
  systemctl --user stop "$UNIT_GREEN" >/dev/null 2>&1 || true
  systemctl --user stop "$UNIT_RED" >/dev/null 2>&1 || true
  systemctl --user reset-failed "$UNIT_GREEN" >/dev/null 2>&1 || true
  systemctl --user reset-failed "$UNIT_RED" >/dev/null 2>&1 || true
  # Best-effort: hard-kill any child PID this run recorded, in case an
  # assertion failed before the normal reap/verify step ran.
  for pf in "$TMP"/*.childpid; do
    [ -f "$pf" ] || continue
    pid="$(cat "$pf" 2>/dev/null || true)"
    if [ -n "${pid:-}" ]; then
      kill -9 "$pid" >/dev/null 2>&1 || true
    fi
  done
  rm -rf "$TMP"
  exit "$ec"
}
trap cleanup EXIT INT TERM

# --- Extract the daemon's ACTUAL rendered KillMode + KillSignal so this
# test exercises the real production config, not a hand-picked value. ---
RENDERED_UNIT="$TMP/rendered.service"
HOME="$TMP/render-home" "$INSTALLER" --render-only --repo "$ROOT" > "$RENDERED_UNIT"
PROD_KILLMODE="$(grep -E '^KillMode=' "$RENDERED_UNIT" | head -1 | cut -d= -f2)"
PROD_KILLSIGNAL="$(grep -E '^KillSignal=' "$RENDERED_UNIT" | head -1 | cut -d= -f2)"

if [ -z "$PROD_KILLMODE" ] || [ -z "$PROD_KILLSIGNAL" ]; then
  echo "FAIL: could not extract KillMode/KillSignal from the rendered production unit template" >&2
  exit 1
fi
echo "INFO: production rendered template: KillMode=$PROD_KILLMODE KillSignal=$PROD_KILLSIGNAL"

# --- parent.sh: the transient service's "main process". On first start it
# forks a detached, long-running child (simulating a live AGY/AO worker
# session process living in the same cgroup) and records its PID. On a
# later restart of the SAME unit, the PIDFILE already exists, so it does
# NOT fork a second child — this lets the test track the fate of the
# ORIGINAL child PID specifically across the restart, instead of losing
# track of identity when the main process gets replaced. ---
cat > "$TMP/parent.sh" <<'PARENT_EOF'
#!/usr/bin/env bash
set -eu
PIDFILE="$1"
if [ ! -f "$PIDFILE" ]; then
  setsid sleep 600 < /dev/null > /dev/null 2>&1 &
  echo "$!" > "$PIDFILE"
fi
exec sleep 300
PARENT_EOF
chmod +x "$TMP/parent.sh"

# run_scenario: spawns a transient unit with the given KillMode, waits for
# the child PID to be recorded, asserts it's alive, restarts the unit, and
# asserts the unit actually respawned a new main process. Leaves the child
# PID in the global CHILD_PID for the caller to make survival/death
# assertions against (the point of this whole test).
run_scenario() {
  local label="$1" unit="$2" killmode="$3" pidfile="$4"

  systemd-run --user --unit="$unit" --collect \
    -p "KillMode=$killmode" \
    -p "KillSignal=$PROD_KILLSIGNAL" \
    -p "TimeoutStopSec=5s" \
    -- /bin/bash "$TMP/parent.sh" "$pidfile" >/dev/null

  local waited=0
  while [ ! -s "$pidfile" ]; do
    sleep 0.2
    waited=$((waited + 1))
    if [ "$waited" -ge 25 ]; then
      echo "FAIL: $label (child PID was never recorded within 5s)"
      FAIL=$((FAIL + 1))
      return 1
    fi
  done
  CHILD_PID="$(cat "$pidfile")"

  if ! kill -0 "$CHILD_PID" 2>/dev/null; then
    echo "FAIL: $label (child PID $CHILD_PID not alive before restart)"
    FAIL=$((FAIL + 1))
    return 1
  fi
  echo "INFO: $label child PID $CHILD_PID alive pre-restart (KillMode=$killmode)"

  local main_pid_before main_pid_after waited2
  main_pid_before="$(systemctl --user show "$unit" --property=MainPID --value)"

  systemctl --user restart "$unit"

  waited2=0
  main_pid_after="0"
  while :; do
    main_pid_after="$(systemctl --user show "$unit" --property=MainPID --value 2>/dev/null || echo 0)"
    if [ -n "$main_pid_after" ] && [ "$main_pid_after" != "0" ] && [ "$main_pid_after" != "$main_pid_before" ]; then
      break
    fi
    waited2=$((waited2 + 1))
    if [ "$waited2" -ge 25 ]; then
      break
    fi
    sleep 0.2
  done

  if [ -z "$main_pid_after" ] || [ "$main_pid_after" = "0" ]; then
    echo "FAIL: $label (unit did not respawn a main process after restart)"
    FAIL=$((FAIL + 1))
    return 1
  fi
  if [ "$main_pid_after" = "$main_pid_before" ]; then
    echo "FAIL: $label (MainPID unchanged across restart, $main_pid_before -- restart did not actually replace the process)"
    FAIL=$((FAIL + 1))
    return 1
  fi
  echo "INFO: $label restart replaced MainPID ($main_pid_before -> $main_pid_after)"
  PASS=$((PASS + 1))
  return 0
}

# ============================================================
# GREEN: production KillMode -- child must SURVIVE restart, and
# must be fully reaped by a DIRECT kill (the ao session-stop path).
# ============================================================
GREEN_PIDFILE="$TMP/green.childpid"
if run_scenario "GREEN (production KillMode=$PROD_KILLMODE)" "$UNIT_GREEN" "$PROD_KILLMODE" "$GREEN_PIDFILE"; then
  GREEN_CHILD_PID="$CHILD_PID"

  if kill -0 "$GREEN_CHILD_PID" 2>/dev/null; then
    echo "PASS: GREEN child PID $GREEN_CHILD_PID SURVIVED daemon restart under KillMode=$PROD_KILLMODE"
    PASS=$((PASS + 1))
  else
    echo "FAIL: GREEN child PID $GREEN_CHILD_PID did NOT survive restart -- KillMode=$PROD_KILLMODE is not preserving sessions as claimed"
    FAIL=$((FAIL + 1))
  fi

  # Simulate the ao Sessions::stop path (adapters.rs:4349): a direct kill(2)
  # on the worker PID, independent of systemd. Assert explicit stop still
  # correctly reaps the process -- no orphan left behind.
  if kill "$GREEN_CHILD_PID" 2>/dev/null; then
    reaped=0
    for _ in $(seq 1 25); do
      if ! kill -0 "$GREEN_CHILD_PID" 2>/dev/null; then
        reaped=1
        break
      fi
      sleep 0.2
    done
    if [ "$reaped" -eq 1 ]; then
      echo "PASS: GREEN explicit direct kill (ao session-stop path, adapters.rs:4349) reaped child PID $GREEN_CHILD_PID -- no orphan remains"
      PASS=$((PASS + 1))
    else
      echo "FAIL: GREEN explicit direct kill did not reap child PID $GREEN_CHILD_PID -- orphan remains after simulated ao session stop"
      FAIL=$((FAIL + 1))
    fi
  else
    echo "FAIL: GREEN direct kill signal to child PID $GREEN_CHILD_PID failed to send"
    FAIL=$((FAIL + 1))
  fi
fi
systemctl --user stop "$UNIT_GREEN" >/dev/null 2>&1 || true

# ============================================================
# RED-proof: pre-fix default KillMode (control-group) -- child MUST die on
# restart. This is the discrimination control: if this scenario's child
# also survived, the GREEN scenario's "survival" result would prove
# nothing about KillMode specifically.
# ============================================================
RED_PIDFILE="$TMP/red.childpid"
if run_scenario "RED (pre-fix default KillMode=control-group)" "$UNIT_RED" "control-group" "$RED_PIDFILE"; then
  RED_CHILD_PID="$CHILD_PID"
  died=0
  for _ in $(seq 1 25); do
    if ! kill -0 "$RED_CHILD_PID" 2>/dev/null; then
      died=1
      break
    fi
    sleep 0.2
  done
  if [ "$died" -eq 1 ]; then
    echo "PASS: RED-proof child PID $RED_CHILD_PID correctly DIED on restart under KillMode=control-group (proves this harness discriminates KillMode behavior; the GREEN survival result is meaningful)"
    PASS=$((PASS + 1))
  else
    echo "FAIL: RED-proof child PID $RED_CHILD_PID SURVIVED restart under KillMode=control-group -- this harness does not discriminate KillMode behavior, GREEN result is not trustworthy"
    FAIL=$((FAIL + 1))
  fi
fi
systemctl --user stop "$UNIT_RED" >/dev/null 2>&1 || true

echo
if [ "$FAIL" -ne 0 ]; then
  echo "FAIL: $FAIL KillMode process-boundary checks failed ($PASS passed)"
  exit 1
fi
echo "PASS: $PASS KillMode process-boundary checks passed"
