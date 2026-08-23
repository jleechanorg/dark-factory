#!/usr/bin/env bash
# test_check_auto_script_configs.sh — TDD coverage for
# scripts/check_auto_script_configs.sh (bead rev-r8vb1): CI enforcement of
# the empty-by-default fail-closed config convention documented in
# docs/automation-config-convention.md.
#
# Verifies:
#   1. Real repo state: daemon/scripts/auto-merge-guard.sh (which already
#      has config/auto_merge_repo_allowlist.json with an empty default)
#      passes the check, run against the actual repo.
#   2. Fixture: an auto-*.sh script with NO matching config reference is
#      FLAGGED (non-zero exit + FAIL line).
#   3. Fixture: an auto-*.sh script whose referenced config ships a
#      non-empty default list is FLAGGED (non-zero exit + FAIL line).
#   4. Fixture: an auto-*.sh script with a properly empty-default config
#      PASSES.
#   5. Fixture: a *-merge-*.sh script matches the same rule as auto-*.sh.
#   6. Dry-run against the full real daemon/scripts/ directory exits 0
#      (existing scripts are not broken by the new check).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CHECK="$ROOT/scripts/check_auto_script_configs.sh"

PASS=0; FAIL=0
ok() { echo "PASS: $1"; PASS=$((PASS + 1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }

assert_exit() {
  local name="$1" expected_rc="$2" actual_rc="$3"
  if [ "$expected_rc" = "$actual_rc" ]; then ok "$name"; else
    bad "$name (expected exit $expected_rc, got $actual_rc)"
  fi
}
assert_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then ok "$name"; else
    bad "$name (expected to find '$needle')"
  fi
}

SCRATCH="$(mktemp -d -t test-check-auto-script-configs.XXXXXX)"
trap 'rm -rf "$SCRATCH"' EXIT

# --- 1. Real repo: auto-merge-guard.sh + gh-pr-merge-wrapper.sh must pass ---
out_real="$(bash "$CHECK" 2>&1)"
rc_real=$?
assert_exit "real daemon/scripts/ exits 0 (existing scripts unbroken)" 0 "$rc_real"
assert_contains "real run: auto-merge-guard.sh PASSes" "PASS: auto-merge-guard.sh" "$out_real"
assert_contains "real run: gh-pr-merge-wrapper.sh is exempted, not flagged" "SKIP (exempt" "$out_real"

# --- Fixture harness: build a scratch repo root with daemon/scripts + config ---
build_fixture_repo() {
  local dir="$1"
  mkdir -p "$dir/daemon/scripts" "$dir/config"
}

# --- 2. auto-*.sh with no config reference at all -> FLAGGED ---
FIX2="$SCRATCH/no-config"
build_fixture_repo "$FIX2"
cat > "$FIX2/daemon/scripts/auto-publish-widget.sh" <<'EOF'
#!/usr/bin/env bash
# fixture: performs an outward-facing publish action with NO allowlist gate.
echo "publishing widget with no gate at all"
EOF
out2="$(bash "$CHECK" "$FIX2/daemon/scripts" "$FIX2" 2>&1)"
rc2=$?
assert_exit "no-config fixture: non-zero exit" 1 "$rc2"
assert_contains "no-config fixture: FAIL line names the script" "FAIL: auto-publish-widget.sh" "$out2"
assert_contains "no-config fixture: FAIL explains missing config reference" "no config/*.json allowlist reference found" "$out2"

# --- 3. auto-*.sh referencing a config with a non-empty default list -> FLAGGED ---
FIX3="$SCRATCH/non-empty-default"
build_fixture_repo "$FIX3"
cat > "$FIX3/daemon/scripts/auto-delete-things.sh" <<'EOF'
#!/usr/bin/env bash
CFG="${DT_POLICY_FILE:-$(git rev-parse --show-toplevel)/config/auto_delete_allowlist.json}"
echo "gated by $CFG"
EOF
cat > "$FIX3/config/auto_delete_allowlist.json" <<'EOF'
{
  "_comment": "should be empty by default but is NOT for this fixture",
  "allowed_repos": ["jleechanorg/dark-factory"]
}
EOF
out3="$(bash "$CHECK" "$FIX3/daemon/scripts" "$FIX3" 2>&1)"
rc3=$?
assert_exit "non-empty-default fixture: non-zero exit" 1 "$rc3"
assert_contains "non-empty-default fixture: FAIL line names the script" "FAIL: auto-delete-things.sh" "$out3"
assert_contains "non-empty-default fixture: FAIL explains non-empty default" "ships a non-empty default list" "$out3"

# --- 4. auto-*.sh referencing a properly empty-default config -> PASS ---
FIX4="$SCRATCH/good"
build_fixture_repo "$FIX4"
cat > "$FIX4/daemon/scripts/auto-archive-things.sh" <<'EOF'
#!/usr/bin/env bash
CFG="${AT_POLICY_FILE:-$(git rev-parse --show-toplevel)/config/auto_archive_allowlist.json}"
echo "gated by $CFG"
EOF
cat > "$FIX4/config/auto_archive_allowlist.json" <<'EOF'
{
  "_comment": "empty by design",
  "allowed_repos": []
}
EOF
out4="$(bash "$CHECK" "$FIX4/daemon/scripts" "$FIX4" 2>&1)"
rc4=$?
assert_exit "empty-default fixture: zero exit" 0 "$rc4"
assert_contains "empty-default fixture: PASS line" "PASS: auto-archive-things.sh" "$out4"

# --- 5. *-merge-*.sh (not auto-*.sh) is matched by the same rule ---
FIX5="$SCRATCH/merge-glob"
build_fixture_repo "$FIX5"
cat > "$FIX5/daemon/scripts/nightly-merge-sweep.sh" <<'EOF'
#!/usr/bin/env bash
echo "merges things with no gate"
EOF
out5="$(bash "$CHECK" "$FIX5/daemon/scripts" "$FIX5" 2>&1)"
rc5=$?
assert_exit "*-merge-*.sh fixture: non-zero exit" 1 "$rc5"
assert_contains "*-merge-*.sh fixture: FAIL line names the script" "FAIL: nightly-merge-sweep.sh" "$out5"

# --- 6. A script matching neither glob is ignored even with no config ---
FIX6="$SCRATCH/irrelevant"
build_fixture_repo "$FIX6"
cat > "$FIX6/daemon/scripts/post-webhook-beacon.sh" <<'EOF'
#!/usr/bin/env bash
echo "not an auto-* or *-merge-* script"
EOF
out6="$(bash "$CHECK" "$FIX6/daemon/scripts" "$FIX6" 2>&1)"
rc6=$?
assert_exit "non-matching script fixture: zero exit (not in scope)" 0 "$rc6"
assert_contains "non-matching script fixture: checked 0 scripts" "checked 0 script(s)" "$out6"

echo ""
echo "=== $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
