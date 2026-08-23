#!/usr/bin/env bash
# test_auto_merge_guard_runner_outage.sh — TDD test for candidate A:
# Avoid future --admin merges by surfacing runner outage earlier.
#
# Proves:
#   1. check_runner_health.sh detects 0 online runners and outputs
#      "RUNNER OUTAGE — consider --admin or wait" with exit code 1.
#   2. check_runner_health.sh detects >0 online runners and outputs
#      the online runner count with exit code 0.
#   3. auto-merge-guard.sh detects 0 online runners when CI is not green,
#      emits the runner outage warning, and posts the PR comment
#      "RUNNER OUTAGE — consider --admin or wait".
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"
CHECK_SCRIPT="$ROOT/scripts/check_runner_health.sh"

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"; FAIL=$((FAIL + 1))
  fi
}
assert_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected to find '$needle')"; FAIL=$((FAIL + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-amg-runner.XXXXXX)"
trap 'rm -rf "$SCRATCH_DIR"' EXIT

FAKE_BIN_DIR="$SCRATCH_DIR/bin"
mkdir -p "$FAKE_BIN_DIR"
FAKE_HOME="$SCRATCH_DIR/home"
mkdir -p "$FAKE_HOME/.dark-factory"

LOG_FILE="$SCRATCH_DIR/gh_calls.log"

cat > "$FAKE_BIN_DIR/gh" <<'EOGH'
#!/usr/bin/env bash
set -u
: "${GH_SHIM_LOG:?GH_SHIM_LOG not set}"
printf '%s\n' "$*" >> "$GH_SHIM_LOG"

if [ "${1:-}" = "api" ] && [ "${2:-}" = "rate_limit" ]; then
  echo '{"resources":{"core":{"remaining":5000},"graphql":{"remaining":5000}}}'
  exit 0
fi

if [ "${1:-}" = "repo" ] && [ "${2:-}" = "view" ]; then
  echo "jleechanorg/dark-factory"
  exit 0
fi

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "list" ]; then
  echo "701 factory/dark-factory-smoke-r1"
  exit 0
fi

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "checks" ]; then
  echo ""
  exit 0
fi

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "view" ]; then
  for a in "$@"; do
    case "$a" in
      *headRefOid*) echo "7017017017017017017017017017017017017017"; exit 0 ;;
      *comments*) echo "[]"; exit 0 ;;
    esac
  done
  echo "{}"
  exit 0
fi

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "comment" ]; then
  echo "comment_posted"
  exit 0
fi

if [ "${1:-}" = "api" ]; then
  endpoint="${2:-}"
  case "$endpoint" in
    *"actions/runners"*)
      echo "${GH_SHIM_ONLINE_RUNNERS:-0}"
      exit 0
      ;;
  esac
fi

echo "{}"
EOGH
chmod +x "$FAKE_BIN_DIR/gh"

echo "=== TEST CASE 1: check_runner_health.sh with 0 online runners ==="
set +e
out1="$(GH_SHIM_LOG="$LOG_FILE" GH_SHIM_ONLINE_RUNNERS="0" PATH="$FAKE_BIN_DIR:$PATH" bash "$CHECK_SCRIPT" 2>&1)"
rc1=$?
set -e

assert "check_runner_health returns exit 1 when 0 online runners" "1" "$rc1"
assert_contains "check_runner_health emits RUNNER OUTAGE message" "RUNNER OUTAGE — consider --admin or wait" "$out1"

echo "=== TEST CASE 2: check_runner_health.sh with 2 online runners ==="
set +e
out2="$(GH_SHIM_LOG="$LOG_FILE" GH_SHIM_ONLINE_RUNNERS="2" PATH="$FAKE_BIN_DIR:$PATH" bash "$CHECK_SCRIPT" 2>&1)"
rc2=$?
set -e

assert "check_runner_health returns exit 0 when runners online" "0" "$rc2"
assert_contains "check_runner_health reports online runner count" "Online runners in jleechanorg/dark-factory pool: 2" "$out2"

echo "=== TEST CASE 3: auto-merge-guard surfaces runner outage on non-green CI ==="
set +e
out3="$(GH_SHIM_LOG="$LOG_FILE" GH_SHIM_ONLINE_RUNNERS="0" HOME="$FAKE_HOME" PATH="$FAKE_BIN_DIR:$PATH" bash "$GUARD" 2>&1)"
set -e

assert_contains "auto-merge-guard surfaces RUNNER OUTAGE in output" "RUNNER OUTAGE — consider --admin or wait" "$out3"
assert_contains "auto-merge-guard posts PR comment for runner outage" "pr comment 701 --repo jleechanorg/dark-factory --body RUNNER OUTAGE — consider --admin or wait" "$(cat "$LOG_FILE")"

echo "=== RESULTS: $PASS passed, $FAIL failed ==="
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
