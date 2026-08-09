#!/usr/bin/env bash
# test_auto_merge_guard_quota_routing.sh — TDD coverage for bead rev-1uno:
# daemon/scripts/auto-merge-guard.sh previously had NO rate_limit preflight
# and made 4 separate GraphQL round-trips per open PR per pass (gh pr list,
# gh pr checks, 2x gh pr view). That GraphQL-only sweep drove the shared org
# graphql quota (user 13840161) to 0/5000, starving every other consumer of
# that quota.
#
# This file proves three contracts against the fixed script:
#   1. BACKOFF: when both core and graphql quotas are critically low, the
#      script must back off the entire pass (zero merge-path gh calls after
#      the preflight) — this is the safety-critical assertion.
#   2. REST ROUTING: when graphql is low but core is healthy, point lookups
#      (single-PR head sha / checks / mergeable) must route to REST, not
#      GraphQL (`gh pr view` must NOT be called for those lookups).
#   3. BASELINE: when both quotas are healthy, the original GraphQL path is
#      unaffected — no backoff, no REST-routing message, exit 0.
#
# Run against the CURRENT (pre-fix) script, test case 1's safety assertion
# FAILS: the unfixed script has no preflight at all and proceeds straight to
# `gh pr list` regardless of quota state. That is the RED proof.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"

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
  if printf '%s' "$haystack" | grep -qF "$needle"; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected to find '$needle')"; FAIL=$((FAIL + 1))
  fi
}
assert_not_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF "$needle"; then
    echo "FAIL: $name (unexpected: found '$needle')"; FAIL=$((FAIL + 1))
  else
    echo "PASS: $name"; PASS=$((PASS + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-amg-quota.XXXXXX)"
trap 'rm -rf "$SCRATCH_DIR"' EXIT

FAKE_BIN_DIR="$SCRATCH_DIR/bin"
mkdir -p "$FAKE_BIN_DIR"
FAKE_HOME="$SCRATCH_DIR/home"
mkdir -p "$FAKE_HOME"

# --- Fake gh shim ------------------------------------------------------
# Logs every invocation (raw args, one call per line) to GH_SHIM_LOG and
# returns canned, already-filtered output for the exact subcommands
# auto-merge-guard.sh issues, so the real GitHub API is never hit.
FAKE_GH="$FAKE_BIN_DIR/gh"
cat > "$FAKE_GH" <<'EOF_GH'
#!/usr/bin/env bash
set -u
: "${GH_SHIM_LOG:?GH_SHIM_LOG not set}"
printf '%s\n' "$*" >> "$GH_SHIM_LOG"

if [ "${1:-}" = "api" ] && [ "${2:-}" = "rate_limit" ]; then
  cat "${GH_SHIM_RATE_LIMIT_JSON:?}"
  exit 0
fi

if [ "${1:-}" = "repo" ] && [ "${2:-}" = "view" ]; then
  echo "jleechanorg/dark-factory"
  exit 0
fi

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "list" ]; then
  cat "${GH_SHIM_PR_LIST_OUT:-/dev/null}"
  exit 0
fi

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "checks" ]; then
  cat "${GH_SHIM_CHECKS_OUT:-/dev/null}"
  exit 0
fi

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "view" ]; then
  for a in "$@"; do
    case "$a" in
      *headRefOid*) echo "${GH_SHIM_HEAD_SHA:-deadbeef}"; exit 0 ;;
      *mergeable*) echo "${GH_SHIM_MERGEABLE_GQL:-MERGEABLE}"; exit 0 ;;
      *state*) echo "${GH_SHIM_STATE_GQL:-OPEN}"; exit 0 ;;
    esac
  done
  echo "{}"; exit 0
fi

if [ "${1:-}" = "pr" ] && [ "${2:-}" = "merge" ]; then
  echo "GH_SHIM: unexpected gh pr merge call" >&2
  exit 1
fi

if [ "${1:-}" = "api" ]; then
  endpoint="${2:-}"
  case "$endpoint" in
    *"/pulls?state=open"*)
      cat "${GH_SHIM_REST_PR_LIST_OUT:-/dev/null}"
      exit 0
      ;;
    *"/check-runs")
      cat "${GH_SHIM_CHECKRUNS_OUT:-/dev/null}"
      exit 0
      ;;
    *"/pulls/"*)
      for a in "$@"; do
        case "$a" in
          *head.sha*) echo "${GH_SHIM_HEAD_SHA:-deadbeef}"; exit 0 ;;
          *mergeable*) echo "${GH_SHIM_MERGEABLE_REST:-true}"; exit 0 ;;
          *merged*) echo "${GH_SHIM_MERGED_REST:-false}"; exit 0 ;;
        esac
      done
      echo "{}"; exit 0
      ;;
  esac
fi

echo "GH_SHIM: unhandled invocation: $*" >&2
exit 1
EOF_GH
chmod +x "$FAKE_GH"

run_guard() {
  # Runs auto-merge-guard.sh with the fake gh on PATH and an isolated HOME
  # (so RATE_FILE=$HOME/.dark-factory/merge-timestamps never touches the
  # real user's rate-limit bookkeeping) and an isolated AFD_LOG (empty —
  # no GATE_ASSESSMENT entries, so any PR that reaches the assessment gate
  # is refused there, never reaching the merge call).
  ( cd "$ROOT" && \
    HOME="$FAKE_HOME" \
    PATH="$FAKE_BIN_DIR:$PATH" \
    AFD_LOG="$SCRATCH_DIR/daemon.jsonl" \
    GH_SHIM_LOG="$GH_SHIM_LOG" \
    GH_SHIM_RATE_LIMIT_JSON="$GH_SHIM_RATE_LIMIT_JSON" \
    GH_SHIM_PR_LIST_OUT="${GH_SHIM_PR_LIST_OUT:-/dev/null}" \
    GH_SHIM_REST_PR_LIST_OUT="${GH_SHIM_REST_PR_LIST_OUT:-/dev/null}" \
    GH_SHIM_CHECKS_OUT="${GH_SHIM_CHECKS_OUT:-/dev/null}" \
    GH_SHIM_CHECKRUNS_OUT="${GH_SHIM_CHECKRUNS_OUT:-/dev/null}" \
    GH_SHIM_HEAD_SHA="${GH_SHIM_HEAD_SHA:-deadbeef}" \
    bash "$GUARD" 2>&1 )
}

echo "=== TEST CASE 1: BACKOFF (both quotas critically low) ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-calls-case1.log"; : > "$GH_SHIM_LOG"
GH_SHIM_RATE_LIMIT_JSON="$SCRATCH_DIR/rate-limit-case1.json"
cat > "$GH_SHIM_RATE_LIMIT_JSON" <<'EOF_RL1'
{"resources":{"core":{"limit":5000,"remaining":50,"reset":9999999999},"graphql":{"limit":5000,"remaining":50,"reset":9999999999}}}
EOF_RL1
GH_SHIM_PR_LIST_OUT="/dev/null"
out1="$(run_guard)"
echo "$out1"
assert_contains "case1: output mentions backing off" "backing off this pass" "$out1"
assert_not_contains "case1: gh pr list (GraphQL) never called after preflight" "pr list" "$(cat "$GH_SHIM_LOG")"
assert_not_contains "case1: gh api pulls?state=open (REST list) never called after preflight" "pulls?state=open" "$(cat "$GH_SHIM_LOG")"
assert_not_contains "case1: gh pr merge never called" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== TEST CASE 2: REST ROUTING (graphql low, core healthy) ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-calls-case2.log"; : > "$GH_SHIM_LOG"
GH_SHIM_RATE_LIMIT_JSON="$SCRATCH_DIR/rate-limit-case2.json"
cat > "$GH_SHIM_RATE_LIMIT_JSON" <<'EOF_RL2'
{"resources":{"core":{"limit":5000,"remaining":4000,"reset":9999999999},"graphql":{"limit":5000,"remaining":50,"reset":9999999999}}}
EOF_RL2
GH_SHIM_REST_PR_LIST_OUT="$SCRATCH_DIR/rest-pr-list-case2.txt"
printf '501 factory/fake-bead-r1\n' > "$GH_SHIM_REST_PR_LIST_OUT"
GH_SHIM_CHECKRUNS_OUT="$SCRATCH_DIR/checkruns-case2.txt"
printf 'build success\n' > "$GH_SHIM_CHECKRUNS_OUT"
GH_SHIM_HEAD_SHA="deadbeefcase2"
out2="$(run_guard)"
echo "$out2"
assert_contains "case2: output mentions routing point lookups to REST" "routing point lookups to REST" "$out2"
call_log2="$(cat "$GH_SHIM_LOG")"
assert_contains "case2: REST pulls endpoint was called for point lookup" "pulls/501" "$call_log2"
assert_not_contains "case2: gh pr view NOT called for point lookups" "pr view 501" "$call_log2"
assert_not_contains "case2: gh pr checks (GraphQL) NOT called" "pr checks" "$call_log2"

echo
echo "=== TEST CASE 3: BASELINE (both quotas healthy, GraphQL path unaffected) ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-calls-case3.log"; : > "$GH_SHIM_LOG"
GH_SHIM_RATE_LIMIT_JSON="$SCRATCH_DIR/rate-limit-case3.json"
cat > "$GH_SHIM_RATE_LIMIT_JSON" <<'EOF_RL3'
{"resources":{"core":{"limit":5000,"remaining":4000,"reset":9999999999},"graphql":{"limit":5000,"remaining":4000,"reset":9999999999}}}
EOF_RL3
GH_SHIM_PR_LIST_OUT="/dev/null"
set +e
out3="$(run_guard)"
rc3=$?
set -e
echo "$out3"
assert "case3: exit code 0" "0" "$rc3"
assert_not_contains "case3: no backoff message" "backing off this pass" "$out3"
assert_not_contains "case3: no REST-routing message" "routing point lookups to REST" "$out3"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
