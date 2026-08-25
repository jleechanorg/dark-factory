#!/usr/bin/env bash
# test_auto_merge_guard_runner_outage.sh — TDD test for candidate A:
# Avoid future --admin merges by surfacing runner outage earlier.
#
# Proves:
#   1. check_runner_health.sh checks org-scoped runners using the configured
#      repo selector and distinguishes fleet-down from drift/probe failure.
#   2. A healthy org pool passes even though the repo-scoped endpoint is empty.
#   3. auto-merge-guard.sh detects a fleet-down condition when CI is not green,
#      emits the runner outage warning, and posts the PR comment
#      without recommending a merge-policy bypass.
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
POLICY_FILE="$SCRATCH_DIR/auto_merge_repo_allowlist.json"
# Current main defaults this allowlist to empty; opt the fixture in so the
# test reaches the post-policy runner-outage probe instead of the policy gate.
printf '%s\n' '{"allowed_repos":["jleechanorg/dark-factory"]}' > "$POLICY_FILE"

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
      *comments*)
        if [ "${GH_SHIM_COMMENTS_FAIL:-0}" = "1" ]; then
          echo "comment lookup failed" >&2
          exit 1
        fi
        if [ "${GH_SHIM_EXISTING_WARNING:-0}" = "1" ]; then
          i=0
          while [ "$i" -lt 500 ]; do echo "unrelated comment $i"; i=$((i + 1)); done
          echo "RUNNER FLEET DOWN — wait for org runner recovery; merge policy remains enforced"
        else
          echo "[]"
        fi
        exit 0
        ;;
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
    "repos/jleechanorg/dark-factory/actions/variables/SELF_HOSTED_RUNNER_LABELS")
      echo '{"value":"[\"self-hosted\",\"self-hosted-mikey\",\"ezgha\"]"}'
      exit 0
      ;;
    "orgs/jleechanorg/actions/runners?per_page=100")
      case "${GH_SHIM_RUNNER_MODE:-down}" in
        healthy)
          echo '{"runners":[{"name":"runner-1","status":"online","busy":false,"labels":[{"name":"self-hosted"},{"name":"self-hosted-mikey"},{"name":"ezgha"}]},{"name":"runner-2","status":"online","busy":true,"labels":[{"name":"self-hosted"},{"name":"self-hosted-mikey"},{"name":"ezgha"}]}]}'
          ;;
        down) echo '{"runners":[]}' ;;
        error) echo 'forbidden' >&2; exit 1 ;;
      esac
      exit 0
      ;;
    "repos/jleechanorg/dark-factory/actions/runners"*)
      echo '{"runners":[]}'
      exit 0
      ;;
  esac
fi

echo "{}"
EOGH
chmod +x "$FAKE_BIN_DIR/gh"

echo "=== TEST CASE 1: org runner fleet down ==="
set +e
out1="$(GH_SHIM_LOG="$LOG_FILE" GH_SHIM_RUNNER_MODE="down" PATH="$FAKE_BIN_DIR:$PATH" bash "$CHECK_SCRIPT" 2>&1)"
rc1=$?
set -e

assert "check_runner_health returns fleet-down exit 3" "3" "$rc1"
assert_contains "check_runner_health emits fleet-down message" "RUNNER FLEET DOWN" "$out1"

echo "=== TEST CASE 2: org-scoped runners healthy while repo endpoint is empty ==="
set +e
out2="$(GH_SHIM_LOG="$LOG_FILE" GH_SHIM_RUNNER_MODE="healthy" PATH="$FAKE_BIN_DIR:$PATH" bash "$CHECK_SCRIPT" 2>&1)"
rc2=$?
set -e

assert "check_runner_health returns exit 0 when runners online" "0" "$rc2"
assert_contains "check_runner_health reports two matching org runners" '"match_count": 2' "$out2"
assert_contains "check_runner_health reports PASS" "Runner selector health: PASS" "$out2"
assert_contains "probe uses org-scoped runners endpoint" "api orgs/jleechanorg/actions/runners?per_page=100" "$(cat "$LOG_FILE")"

echo "=== TEST CASE 3: auto-merge-guard surfaces runner outage on non-green CI ==="
set +e
out3="$(GH_SHIM_LOG="$LOG_FILE" GH_SHIM_RUNNER_MODE="down" AMG_REPO_POLICY_FILE="$POLICY_FILE" HOME="$FAKE_HOME" PATH="$FAKE_BIN_DIR:$PATH" bash "$GUARD" 2>&1)"
set -e

assert_contains "auto-merge-guard surfaces fleet-down output" "RUNNER FLEET DOWN" "$out3"
assert_contains "auto-merge-guard posts fleet-down PR comment" "pr comment 701 --repo jleechanorg/dark-factory --body RUNNER FLEET DOWN — wait for org runner recovery; merge policy remains enforced" "$(cat "$LOG_FILE")"
if printf '%s\n%s' "$out3" "$(cat "$LOG_FILE")" | grep -q -- '--admin'; then
  echo "FAIL: runner guidance must not recommend --admin"; FAIL=$((FAIL + 1))
else
  echo "PASS: runner guidance does not recommend --admin"; PASS=$((PASS + 1))
fi

echo "=== TEST CASE 4: API/auth failure is inconclusive, not an outage ==="
set +e
out4="$(GH_SHIM_LOG="$LOG_FILE" GH_SHIM_RUNNER_MODE="error" PATH="$FAKE_BIN_DIR:$PATH" bash "$CHECK_SCRIPT" 2>&1)"
rc4=$?
set -e
assert "check_runner_health returns invocation exit 2" "2" "$rc4"
assert_contains "probe failure is explicitly inconclusive" "RUNNER STATUS INCONCLUSIVE" "$out4"
if printf '%s' "$out4" | grep -q "RUNNER FLEET DOWN"; then
  echo "FAIL: API failure must not be classified as fleet down"; FAIL=$((FAIL + 1))
else
  echo "PASS: API failure is not classified as fleet down"; PASS=$((PASS + 1))
fi

echo "=== TEST CASE 5: failed comment lookup never posts a duplicate ==="
LOG_FILE5="$SCRATCH_DIR/gh_calls_case5.log"
set +e
out5="$(GH_SHIM_LOG="$LOG_FILE5" GH_SHIM_RUNNER_MODE="down" GH_SHIM_COMMENTS_FAIL="1" AMG_REPO_POLICY_FILE="$POLICY_FILE" HOME="$FAKE_HOME" PATH="$FAKE_BIN_DIR:$PATH" bash "$GUARD" 2>&1)"
set -e
assert_contains "failed comment lookup is inconclusive" "RUNNER WARNING DEDUP INCONCLUSIVE" "$out5"
if grep -q "pr comment" "$LOG_FILE5"; then
  echo "FAIL: failed comment lookup must not post"; FAIL=$((FAIL + 1))
else
  echo "PASS: failed comment lookup does not post"; PASS=$((PASS + 1))
fi

echo "=== TEST CASE 6: existing warning in large output remains deduplicated ==="
LOG_FILE6="$SCRATCH_DIR/gh_calls_case6.log"
set +e
GH_SHIM_LOG="$LOG_FILE6" GH_SHIM_RUNNER_MODE="down" GH_SHIM_EXISTING_WARNING="1" AMG_REPO_POLICY_FILE="$POLICY_FILE" HOME="$FAKE_HOME" PATH="$FAKE_BIN_DIR:$PATH" bash "$GUARD" >/dev/null 2>&1
set -e
if grep -q "pr comment" "$LOG_FILE6"; then
  echo "FAIL: existing large-output warning must not be duplicated"; FAIL=$((FAIL + 1))
else
  echo "PASS: existing large-output warning is not duplicated"; PASS=$((PASS + 1))
fi

echo "=== RESULTS: $PASS passed, $FAIL failed ==="
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
