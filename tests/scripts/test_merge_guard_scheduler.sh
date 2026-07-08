#!/usr/bin/env bash
# test_merge_guard_scheduler.sh — verify the auto-merge-guard.sh scheduler
# (systemd user timer + launchd plist template) parses cleanly and points at
# the hardened auto-merge-guard.sh that already implements the X4 controls
# (no-red gate, head-SHA check via gh pr view, per-hour budget).
#
# This is the parse-only test for the scheduler layer (jleechan-s3c). It does
# NOT run a live merge and does NOT touch the daemon src/ tree.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PASS=0
FAIL=0

assert_file_exists() {
  local name="$1" path="$2"
  if [ -f "$path" ]; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (missing: $path)"
    FAIL=$((FAIL + 1))
  fi
}

assert_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file"; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (missing pattern: $pattern in $file)"
    FAIL=$((FAIL + 1))
  fi
}

assert_not_grep() {
  local name="$1" pattern="$2" file="$3"
  if grep -qE "$pattern" "$file"; then
    echo "FAIL: $name (unexpected pattern: $pattern in $file)"
    FAIL=$((FAIL + 1))
  else
    echo "PASS: $name"
    PASS=$((PASS + 1))
  fi
}

# -- 1. Unit files exist ----------------------------------------------------
SVC="$ROOT/daemon/scripts/systemd-user/dark-factory-merge-guard.service"
TMR="$ROOT/daemon/scripts/systemd-user/dark-factory-merge-guard.timer"
PLIST_TPL="$ROOT/daemon/launchd/opt-in/ai.dark-factory.auto-merge-guard.plist.template"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"

assert_file_exists "systemd service file present" "$SVC"
assert_file_exists "systemd timer file present"   "$TMR"
assert_file_exists "launchd plist template present" "$PLIST_TPL"

# -- 2. auto-merge-guard.sh still parses and claims no-red policy ------------
assert_file_exists "auto-merge-guard.sh present" "$GUARD"
if [ -f "$GUARD" ]; then
  if bash -n "$GUARD"; then
    echo "PASS: auto-merge-guard.sh bash -n clean"
    PASS=$((PASS + 1))
  else
    echo "FAIL: auto-merge-guard.sh bash -n failed"
    FAIL=$((FAIL + 1))
  fi
  assert_grep "guard header advertises no-red policy" "no-red" "$GUARD"
fi

# -- 3. systemd service + timer schema checks -------------------------------
if [ -f "$SVC" ]; then
  assert_grep "service is Type=oneshot"            '^Type=oneshot$'             "$SVC"
  assert_grep "service ExecStart targets auto-merge-guard.sh" 'auto-merge-guard\.sh' "$SVC"
fi
if [ -f "$TMR" ]; then
  assert_grep "timer has OnUnitActiveSec"           '^OnUnitActiveSec='          "$TMR"
  assert_grep "timer Persistent=true"               '^Persistent=true$'          "$TMR"
  assert_grep "timer Unit references the service"   '^Unit=dark-factory-merge-guard\.service$' "$TMR"
  assert_grep "timer Install [Install] WantedBy=timers.target" '^WantedBy=timers\.target$' "$TMR"
fi

# -- 4. systemd-analyze verify on both unit files (best-effort) --------------
if command -v systemd-analyze >/dev/null 2>&1; then
  TMP="$(mktemp -d -t dark-factory-scheduler-test.XXXXXX)"
  cp "$SVC" "$TMP/s.service" 2>/dev/null || true
  cp "$TMR" "$TMP/s.timer"   2>/dev/null || true
  if [ -f "$TMP/s.timer" ]; then
    if systemd-analyze verify "$TMP/s.timer" >/dev/null 2>&1; then
      echo "PASS: systemd-analyze verify passes for timer"
      PASS=$((PASS + 1))
    else
      echo "FAIL: systemd-analyze verify failed for timer"
      systemd-analyze verify "$TMP/s.timer" 2>&1 | sed 's/^/    /'
      FAIL=$((FAIL + 1))
    fi
  fi
  rm -rf "$TMP"
else
  echo "SKIP: systemd-analyze not available (not Linux?)"
fi

# -- 5. launchd plist template parses (plutil or python plistlib fallback) ---
if [ -f "$PLIST_TPL" ]; then
  RENDERED="$(mktemp -t dark-factory-plist-rendered.XXXXXX.plist)"
  # Render @HOME@ -> a fake path so plutil sees a well-formed plist.
  sed -e "s%@HOME@%/tmp/fake-home-for-lint%g" "$PLIST_TPL" > "$RENDERED"
  if command -v plutil >/dev/null 2>&1; then
    if plutil -lint "$RENDERED" >/dev/null 2>&1; then
      echo "PASS: plutil -lint passes for rendered plist"
      PASS=$((PASS + 1))
    else
      echo "FAIL: plutil -lint failed for rendered plist"
      plutil -lint "$RENDERED" 2>&1 | sed 's/^/    /'
      FAIL=$((FAIL + 1))
    fi
  elif python3 -c "import plistlib,sys; plistlib.load(open(sys.argv[1],'rb'))" "$RENDERED" >/dev/null 2>&1; then
    echo "PASS: python plistlib parses rendered plist"
    PASS=$((PASS + 1))
  else
    echo "FAIL: no plutil or python plistlib to validate"
    FAIL=$((FAIL + 1))
  fi
  rm -f "$RENDERED"

  assert_grep     "plist StartInterval=60"         '^[[:space:]]*<integer>60</integer>[[:space:]]*$' "$PLIST_TPL"
  assert_grep     "plist RunAtLoad present"        '^[[:space:]]*<key>RunAtLoad</key>' "$PLIST_TPL"
  assert_grep     "plist @HOME@ is template"       '@HOME@' "$PLIST_TPL"
  assert_grep     "plist points at auto-merge-guard.sh" 'auto-merge-guard\.sh' "$PLIST_TPL"
  assert_not_grep "plist no leftover @TICK_INTERVAL@" '@TICK_INTERVAL@' "$PLIST_TPL"
fi

# -- 6. Installer is idempotent + targets both Linux and macOS ---------------
INSTALLER=""
if [ -f "$ROOT/scripts/install-merge-guard.sh" ]; then
  INSTALLER="$ROOT/scripts/install-merge-guard.sh"
elif [ -f "$ROOT/install-merge-guard.sh" ]; then
  INSTALLER="$ROOT/install-merge-guard.sh"
fi
assert_file_exists "merge-guard installer script present" "${INSTALLER:-/nonexistent}"
if [ -n "$INSTALLER" ] && [ -f "$INSTALLER" ]; then
  if bash -n "$INSTALLER"; then
    echo "PASS: installer bash -n clean"
    PASS=$((PASS + 1))
  else
    echo "FAIL: installer bash -n failed"
    FAIL=$((FAIL + 1))
  fi
  assert_grep "installer copies systemd-user files"   'systemd/user' "$INSTALLER"
  assert_grep "installer calls daemon-reload"         'daemon-reload' "$INSTALLER"
  assert_grep "installer enables timer"               'systemctl --user enable --now' "$INSTALLER"
  assert_grep "installer references timer unit name"  'dark-factory-merge-guard\.timer' "$INSTALLER"
  assert_grep "installer handles macOS branch"        'darwin|Darwin|macos|macOS' "$INSTALLER"
fi

# -- 7. install-launchagents.sh --dry-run must NOT bootstrap merge-guard ------
INSTALL_LAUNCHAGENTS="$ROOT/install-launchagents.sh"
if [ -f "$INSTALL_LAUNCHAGENTS" ]; then
  DRYRUN_OUT="$(cd "$ROOT" && "$INSTALL_LAUNCHAGENTS" --dry-run 2>&1 || true)"
  if echo "$DRYRUN_OUT" | grep -q "auto-merge-guard"; then
    echo "FAIL: install-launchagents.sh --dry-run references auto-merge-guard (opt-in bypass regression)"
    echo "$DRYRUN_OUT" | grep "auto-merge-guard" | sed 's/^/    /'
    FAIL=$((FAIL + 1))
  else
    echo "PASS: install-launchagents.sh --dry-run does not reference auto-merge-guard"
    PASS=$((PASS + 1))
  fi
else
  echo "SKIP: install-launchagents.sh not found at $INSTALL_LAUNCHAGENTS"
fi

# -- Summary ----------------------------------------------------------------
if [ "$FAIL" -ne 0 ]; then
  echo "FAIL: $FAIL scheduler checks failed (PASS: $PASS)"
  exit 1
fi

echo "PASS: $PASS scheduler checks passed"