#!/usr/bin/env bash
# test_callpath_overlay_harness.sh — verify the vendored overlay-harness probe
# (bin/overlay-harness-check.sh) reports GREEN when factory-overlay.sh has
# all required subcommands, and RED with the missing subcommand name when one
# is removed.
#
# Self-contained: depends only on bin/overlay-harness-check.sh which lives in
# this repo (NOT user-scope ~/.claude/skills/callpath/profiles/dark-factory/run.sh
# as in the original PR #170 draft). See bead jleechan-8xxl.
#
# Strategy: shell-stub daemon/factory-overlay.sh under a temporary DARK_FACTORY_HOME,
# redirect the rust daemon + CXDB + config paths into the same temp directory, and
# confirm overlay-harness-check.sh reports the expected GREEN/RED status.
#
# Run with: bash tests/scripts/test_callpath_overlay_harness.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROBE="$ROOT/bin/overlay-harness-check.sh"
TESTS_DIR="$(dirname "$0")"

PASS=0
FAIL=0

# Expected subcommands must match the vendored probe (bin/overlay-harness-check.sh).
EXPECTED_SUBCOMMANDS=(
  init
  intake-upsert
  route-record
  capacity
  dispatch-record
  pr-opened
  autonomy-tick
  gate-assessment
  prev-gate-assessment
  ready
  reroll-verdict
  park
  park-duplicate
  bead-closed-check
  tick-summary
  recover-held
  unstick-dispatching
  redrive-pr
  list
)

assert() {
    local name="$1" expected="$2" actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        echo "PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "FAIL: $name (expected '$expected', got '$actual')"
        FAIL=$((FAIL + 1))
    fi
}

if [[ ! -x "$PROBE" ]]; then
    echo "FAIL: vendored probe not found or not executable: $PROBE" >&2
    exit 1
fi

# Build a tiny factory-overlay.sh shell stub with the requested set of
# subcommand case branches. Each subcommand becomes a no-op `echo ok`.
# Match the real overlay.sh shape: case branches start at column 0 so that
# the probe's `grep -qE "^${sub}\)"` matches verbatim.
write_overlay_stub() {
    local out="$1"
    shift
    local sub
    {
        echo '#!/usr/bin/env bash'
        echo '# shell-stub for test_callpath_overlay_harness.sh'
        echo 'set -euo pipefail'
        echo 'case "${1:-}" in'
        for sub in "$@"; do
            printf '%s)\n  echo ok\n  ;;\n' "$sub"
        done
        echo '*)'
        echo '  echo "unknown: ${1:-}"'
        echo '  exit 1'
        echo '  ;;'
        echo 'esac'
    } > "$out"
    chmod +x "$out"
}

setup_temp_home() {
    local tmp
    tmp="$(mktemp -d -t callpath-overlay-test.XXXXXX)"
    mkdir -p "$tmp/daemon"
    echo "$tmp"
}

teardown_temp_home() {
    local tmp="$1"
    [[ -n "$tmp" && "$tmp" == */callpath-overlay-test.* ]] && rm -rf "$tmp"
}

# ---------------------------------------------------------------------------
# Test 1 — all 19 subcommands present → overlay-harness: ok/19
# ---------------------------------------------------------------------------
tmp1="$(setup_temp_home)"
stub="$tmp1/daemon/factory-overlay.sh"
write_overlay_stub "$stub" "${EXPECTED_SUBCOMMANDS[@]}"

# Sanity: stub must be executable and contain every subcommand.
if [[ ! -x "$stub" ]]; then
    echo "FAIL: stub is not executable"
    FAIL=$((FAIL + 1))
fi
missing_in_stub=""
for sub in "${EXPECTED_SUBCOMMANDS[@]}"; do
    if ! grep -q "^${sub})" "$stub"; then
        missing_in_stub="$missing_in_stub $sub"
    fi
done
if [[ -n "$missing_in_stub" ]]; then
    echo "FAIL: stub missing expected subcommands:$missing_in_stub"
    FAIL=$((FAIL + 1))
fi

probe_out="$(bash "$PROBE" "$stub" 2>/dev/null || true)"
assert "all 19 subcommands → ok/19" "ok/19" "$probe_out"
teardown_temp_home "$tmp1"

# ---------------------------------------------------------------------------
# Test 2 — one subcommand missing → overlay-harness: missing:<sub>
# ---------------------------------------------------------------------------
missing_sub="reroll-verdict"
tmp2="$(setup_temp_home)"
stub="$tmp2/daemon/factory-overlay.sh"
stub_subs=()
for sub in "${EXPECTED_SUBCOMMANDS[@]}"; do
    [[ "$sub" == "$missing_sub" ]] && continue
    stub_subs+=("$sub")
done
write_overlay_stub "$stub" "${stub_subs[@]}"

# Sanity: the missing subcommand must NOT be present.
if grep -q "^${missing_sub})" "$stub"; then
    echo "FAIL: stub for missing test still contains $missing_sub"
    FAIL=$((FAIL + 1))
fi

probe_out="$(bash "$PROBE" "$stub" 2>/dev/null || true)"
expected_missing="missing:${missing_sub}"
assert "missing subcommand reports name" "$expected_missing" "$probe_out"
teardown_temp_home "$tmp2"

# ---------------------------------------------------------------------------
# Test 3 — overlay script missing entirely → overlay-harness: missing:overlay
# ---------------------------------------------------------------------------
tmp3="$(setup_temp_home)"
# Do NOT create any daemon/factory-overlay.sh — just an empty daemon dir.
mkdir -p "$tmp3/daemon"
probe_out="$(bash "$PROBE" "$tmp3/daemon/factory-overlay.sh" 2>/dev/null || true)"
assert "missing overlay file → missing:overlay" "missing:overlay" "$probe_out"
teardown_temp_home "$tmp3"

echo ""
echo "=========================================="
echo "PASS=$PASS FAIL=$FAIL"
echo "=========================================="
[[ "$FAIL" -eq 0 ]] || exit 1