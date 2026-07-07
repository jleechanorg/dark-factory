#!/usr/bin/env bash
# test_callpath_overlay_harness.sh — verify callpath profile dark-factory
# overlay-harness layer probes factory-overlay.sh for the 19 required
# subcommands (bead jleechan-df94; replaces decommissioned factory-lite-harness.sh
# probes from commit e60b5a31b).
#
# Strategy: shell-stub daemon/factory-overlay.sh under a temporary
# DARK_FACTORY_HOME, redirect the rust daemon + CXDB + config paths into the
# same temp directory, and confirm `callpath run dark-factory` reports
# overlay-harness: GREEN with all 19 subcommands present, or RED with the
# missing subcommand name when one is removed.
#
# Run with: bash tests/scripts/test_callpath_overlay_harness.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CALLPATH="${CALLPATH:-callpath}"
TESTS_DIR="$(dirname "$0")"

PASS=0
FAIL=0

# Subcommands required by bead jleechan-df94 (PR #167 10dc5b16a).
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

# Build a tiny factory-overlay.sh shell stub with the requested set of
# subcommand case branches. Each subcommand becomes a no-op `echo ok`.
# Match the real overlay.sh shape: case branches start at column 0 so that
# the callpath probe `grep -q "^${sub})"` matches verbatim.
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

# Run callpath against a fake DARK_FACTORY_HOME (with a stub daemon/) and
# capture only the overlay-harness line. Returns 0 if that line contains the
# expected token (GREEN|RED|.../...).
run_overlay_harness_probe() {
  local fake_home="$1"
  local marker="$2"
  local out line
  out="$(DARK_FACTORY_HOME="$fake_home" \
         DAEMON_CXDB="$fake_home/daemon-cxdb.sqlite" \
         BR_DB="$fake_home/beads.db" \
         "$CALLPATH" run dark-factory 2>/dev/null || true)"
  line="$(echo "$out" | grep -E "^\s*overlay-harness:" || true)"
  # Strip leading whitespace so assertions are exact.
  line="${line#"${line%%[![:space:]]*}"}"
  echo "$line" | grep -F "$marker" > /dev/null
}

# Capture just the overlay-harness line (with leading whitespace stripped).
overlay_harness_line() {
  local fake_home="$1"
  local out line
  out="$(DARK_FACTORY_HOME="$fake_home" \
         DAEMON_CXDB="$fake_home/daemon-cxdb.sqlite" \
         BR_DB="$fake_home/beads.db" \
         "$CALLPATH" run dark-factory 2>/dev/null || true)"
  line="$(echo "$out" | grep -E "^\s*overlay-harness:" || true)"
  line="${line#"${line%%[![:space:]]*}"}"
  echo "$line"
}

# Workaround for callpath binary bug: when DARK_FACTORY_HOME is unset, the
# profile.yaml default `${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}` is
# returned as literal `$HOME/projects/dark-factory` (HOME is not expanded).
# Force DARK_FACTORY_HOME explicitly so the run.sh resolves the stub.

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
# Test 1 — all 19 subcommands present → overlay-harness: GREEN
# ---------------------------------------------------------------------------
tmp1="$(setup_temp_home)"
stub="$tmp1/daemon/factory-overlay.sh"
write_overlay_stub "$stub" "${EXPECTED_SUBCOMMANDS[@]}"

# Sanity: stub must be executable and contain every subcommand
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

if run_overlay_harness_probe "$tmp1" "overlay-harness: GREEN (factory-overlay.sh; ok/19)"; then
  echo "PASS: all 19 subcommands → overlay-harness: GREEN (factory-overlay.sh; ok/19)"
  PASS=$((PASS + 1))
else
  echo "FAIL: all 19 subcommands did not produce GREEN"
  FAIL=$((FAIL + 1))
  DARK_FACTORY_HOME="$tmp1" DAEMON_CXDB="$tmp1/daemon-cxdb.sqlite" BR_DB="$tmp1/beads.db" \
    "$CALLPATH" run dark-factory 2>/dev/null | grep -E "overlay-harness:" || true
fi
teardown_temp_home "$tmp1"

# ---------------------------------------------------------------------------
# Test 2 — one subcommand missing → overlay-harness: RED with the missing name
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

# Sanity: the missing subcommand must NOT be present, all others must be
if grep -q "^${missing_sub})" "$stub"; then
  echo "FAIL: stub for missing test still contains $missing_sub"
  FAIL=$((FAIL + 1))
fi

# capture line for diagnostics
line="$(overlay_harness_line "$tmp2")"

expected_line="overlay-harness: RED (missing:${missing_sub})"
assert "missing subcommand produces RED with name" "$expected_line" "$line"
# Belt-and-braces: the line must mention the missing subcommand name regardless
# of formatting.
if echo "$line" | grep -qF "$missing_sub"; then
  echo "PASS: missing-subcommand line includes subcommand name"
  PASS=$((PASS + 1))
else
  echo "FAIL: missing-subcommand line did not include '$missing_sub' (got: '$line')"
  FAIL=$((FAIL + 1))
fi
teardown_temp_home "$tmp2"

# ---------------------------------------------------------------------------
# Test 3 — overlay script missing entirely → overlay-harness: RED
# (this is the original failure mode from e60b5a31b)
# ---------------------------------------------------------------------------
tmp3="$(setup_temp_home)"
# Do NOT create any daemon/factory-overlay.sh — just an empty daemon dir
mkdir -p "$tmp3/daemon"
line="$(overlay_harness_line "$tmp3")"
expected_line3="overlay-harness: RED (missing:overlay)"
assert "missing overlay file produces RED" "$expected_line3" "$line"
teardown_temp_home "$tmp3"

echo ""
echo "=========================================="
echo "PASS=$PASS FAIL=$FAIL"
echo "=========================================="
[[ "$FAIL" -eq 0 ]] || exit 1
