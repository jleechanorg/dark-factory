#!/usr/bin/env bash
# test_systemd_user_install.sh — verify the Linux systemd user unit renders to
# the Rust daemon, with failure restart + watchdog supervision, without mutating the host.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
INSTALLER="$ROOT/daemon/systemd/install-systemd-user.sh"
TMP="$(mktemp -d -t dark-factory-systemd-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

PASS=0
FAIL=0

assert_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file"; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (missing pattern: $pattern)"
    FAIL=$((FAIL + 1))
  fi
}

assert_not_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file"; then
    echo "FAIL: $name (unexpected pattern: $pattern)"
    FAIL=$((FAIL + 1))
  else
    echo "PASS: $name"
    PASS=$((PASS + 1))
  fi
}

UNIT="$TMP/unit.service"
HOME="$TMP/home" "$INSTALLER" --render-only --repo "$ROOT" > "$UNIT"

assert_grep "unit uses notify protocol" '^Type=notify$' "$UNIT"
assert_grep "unit allows main process notify access" '^NotifyAccess=main$' "$UNIT"
assert_grep "unit restarts daemon on failure" '^Restart=on-failure$' "$UNIT"
assert_grep "unit has watchdog sized for long LLM ticks" '^WatchdogSec=600$' "$UNIT"
assert_grep "unit includes npm global tools on PATH" '\.npm-global/bin' "$UNIT"
assert_grep "unit includes nvm node 22 tools on PATH" '\.nvm/versions/node/v22\.22\.0/bin' "$UNIT"
assert_grep "unit runs release Rust daemon" "^ExecStart=$ROOT/daemon/target/release/daemon$" "$UNIT"
assert_grep "unit pins working directory" "^WorkingDirectory=$ROOT$" "$UNIT"
assert_not_grep "unit does not run legacy shell tick lane" 'factory-af-tick|factory-tick|launchd' "$UNIT"

DRY="$TMP/dry-run.txt"
HOME="$TMP/home" SYSTEMD_USER_DIR="$TMP/systemd-user" \
  "$INSTALLER" --dry-run --skip-build --repo "$ROOT" > "$DRY"
assert_grep "dry-run would reload user systemd" 'systemctl --user daemon-reload' "$DRY"
assert_grep "dry-run would check linger" 'loginctl show-user' "$DRY"
assert_grep "dry-run would enable linger if needed" 'loginctl enable-linger' "$DRY"
assert_grep "dry-run would enable and start service" 'systemctl --user enable --now ai.dark-factory.daemon.service' "$DRY"

if [ "$FAIL" -ne 0 ]; then
  echo "FAIL: $FAIL systemd installer checks failed"
  exit 1
fi

echo "PASS: $PASS systemd installer checks passed"
