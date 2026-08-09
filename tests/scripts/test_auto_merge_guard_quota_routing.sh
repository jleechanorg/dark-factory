#!/usr/bin/env bash
# test_auto_merge_guard_quota_routing.sh — TDD coverage for bead rev-1uno:
# daemon/scripts/auto-merge-guard.sh previously had NO rate_limit preflight
# and made 4 separate GraphQL round-trips per open PR per pass (gh pr list,
# gh pr checks, 2x gh pr view). That GraphQL-only sweep drove the shared org
# graphql quota (user 13840161) to 0/5000, starving every other consumer of
# that quota.
#
# This file proves three quota-routing contracts against the fixed script:
#   1. BACKOFF: when both core and graphql quotas are critically low, the
#      script must back off the entire pass (zero merge-path gh calls after
#      the preflight) — this is the safety-critical assertion.
#   2. REST ROUTING: when graphql is low but core is healthy, point lookups
#      (single-PR head sha / checks / mergeable) must route to REST, not
#      GraphQL (`gh pr view` must NOT be called for those lookups).
#   3. BASELINE: when both quotas are healthy, the original GraphQL path is
#      unaffected — no backoff, no REST-routing message, exit 0.
#
# Cases 4-7 are a codex-skeptic-review follow-up (PRs #619/#620/#621
# REQUEST_CHANGES): the checks-evaluation block only grepped raw check text
# for "pending|queued|in_progress" (not-yet-green) or "fail" (red). That
# missed CANCELLED/TIMED_OUT conclusions, had no REST pagination, and — most
# dangerously — treated an EMPTY check-run list as "green" (neither grep
# matched empty output). Cases 4-7 prove the checks_all_green() fix closes
# all of that:
#   4. CANCELLED conclusion (status completed) must NOT be treated as green.
#   5. An EMPTY check-run list must NOT be treated as green.
#   6. A bad run visible only via pagination (a naive single-page-of-30
#      fetch would miss it) must still be caught — proves --paginate is
#      actually used, not just requested.
#   7. A failing legacy commit-status (separate from check-runs, all of
#      which are green) must still block the merge.
#
# Run against the CURRENT (pre-fix) script:
#   - test case 1's safety assertion FAILS: the unfixed script has no
#     preflight at all and proceeds straight to `gh pr list` regardless of
#     quota state.
#   - test cases 4 and 5 FAIL: the unfixed script's two greps
#     (pending|queued|in_progress, fail) neither match a lone "cancelled"
#     conclusion nor empty check output, so the unfixed script proceeds past
#     the checks gate and attempts the assessment/mergeable/merge path
#     instead of skipping.
# That is the RED proof.
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
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected to find '$needle')"; FAIL=$((FAIL + 1))
  fi
}
assert_not_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
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
      # Distinguish "the real invocation requested --paginate" from a naive
      # single-page fetch: --paginate serves the FULL canned content (which
      # may include a check-run that would only be visible past page 1 of a
      # 30-item default page size); its absence serves the PAGE1-only
      # content. Falls back to the legacy GH_SHIM_CHECKRUNS_OUT var (used by
      # cases 1-5, 7) when the paginate-specific vars are unset, so existing
      # cases don't need to change.
      _paginate=0
      for a in "$@"; do
        case "$a" in --paginate) _paginate=1 ;; esac
      done
      if [ "$_paginate" -eq 1 ]; then
        cat "${GH_SHIM_CHECKRUNS_FULL_OUT:-${GH_SHIM_CHECKRUNS_OUT:-/dev/null}}"
      else
        cat "${GH_SHIM_CHECKRUNS_PAGE1_OUT:-${GH_SHIM_CHECKRUNS_OUT:-/dev/null}}"
      fi
      exit 0
      ;;
    *"/status")
      cat "${GH_SHIM_COMMIT_STATUS_OUT:-/dev/null}"
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
    GH_SHIM_CHECKRUNS_FULL_OUT="${GH_SHIM_CHECKRUNS_FULL_OUT:-}" \
    GH_SHIM_CHECKRUNS_PAGE1_OUT="${GH_SHIM_CHECKRUNS_PAGE1_OUT:-}" \
    GH_SHIM_COMMIT_STATUS_OUT="${GH_SHIM_COMMIT_STATUS_OUT:-/dev/null}" \
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
printf 'completed success\n' > "$GH_SHIM_CHECKRUNS_OUT"
GH_SHIM_CHECKRUNS_FULL_OUT=""
GH_SHIM_CHECKRUNS_PAGE1_OUT=""
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
echo "=== TEST CASE 4: CANCELLED check-run conclusion must NOT be green (codex follow-up) ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-calls-case4.log"; : > "$GH_SHIM_LOG"
GH_SHIM_RATE_LIMIT_JSON="$SCRATCH_DIR/rate-limit-case4.json"
cat > "$GH_SHIM_RATE_LIMIT_JSON" <<'EOF_RL4'
{"resources":{"core":{"limit":5000,"remaining":4000,"reset":9999999999},"graphql":{"limit":5000,"remaining":50,"reset":9999999999}}}
EOF_RL4
GH_SHIM_REST_PR_LIST_OUT="$SCRATCH_DIR/rest-pr-list-case4.txt"
printf '601 factory/fake-bead-cancelled\n' > "$GH_SHIM_REST_PR_LIST_OUT"
GH_SHIM_CHECKRUNS_OUT="$SCRATCH_DIR/checkruns-case4.txt"
printf 'completed cancelled\n' > "$GH_SHIM_CHECKRUNS_OUT"
GH_SHIM_CHECKRUNS_FULL_OUT=""
GH_SHIM_CHECKRUNS_PAGE1_OUT=""
GH_SHIM_COMMIT_STATUS_OUT="/dev/null"
GH_SHIM_HEAD_SHA="deadbeefcase4"
out4="$(run_guard)"
echo "$out4"
assert_contains "case4: output shows CI not green for cancelled run" "CI not green" "$out4"
assert_not_contains "case4: gh pr merge NEVER called (cancelled conclusion)" "pr merge" "$(cat "$GH_SHIM_LOG")"
assert_not_contains "case4: gh api ...merge NEVER called (cancelled conclusion)" "-X merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== TEST CASE 5: EMPTY check-run list must NOT be green (codex follow-up) ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-calls-case5.log"; : > "$GH_SHIM_LOG"
GH_SHIM_RATE_LIMIT_JSON="$SCRATCH_DIR/rate-limit-case5.json"
cat > "$GH_SHIM_RATE_LIMIT_JSON" <<'EOF_RL5'
{"resources":{"core":{"limit":5000,"remaining":4000,"reset":9999999999},"graphql":{"limit":5000,"remaining":50,"reset":9999999999}}}
EOF_RL5
GH_SHIM_REST_PR_LIST_OUT="$SCRATCH_DIR/rest-pr-list-case5.txt"
printf '602 factory/fake-bead-empty\n' > "$GH_SHIM_REST_PR_LIST_OUT"
GH_SHIM_CHECKRUNS_OUT="/dev/null"
GH_SHIM_CHECKRUNS_FULL_OUT=""
GH_SHIM_CHECKRUNS_PAGE1_OUT=""
GH_SHIM_COMMIT_STATUS_OUT="/dev/null"
GH_SHIM_HEAD_SHA="deadbeefcase5"
out5="$(run_guard)"
echo "$out5"
assert_contains "case5: output shows CI not green for empty check-run list" "CI not green" "$out5"
assert_not_contains "case5: gh pr merge NEVER called (empty check-runs)" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== TEST CASE 6: bad run only visible via pagination must still be caught ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-calls-case6.log"; : > "$GH_SHIM_LOG"
GH_SHIM_RATE_LIMIT_JSON="$SCRATCH_DIR/rate-limit-case6.json"
cat > "$GH_SHIM_RATE_LIMIT_JSON" <<'EOF_RL6'
{"resources":{"core":{"limit":5000,"remaining":4000,"reset":9999999999},"graphql":{"limit":5000,"remaining":50,"reset":9999999999}}}
EOF_RL6
GH_SHIM_REST_PR_LIST_OUT="$SCRATCH_DIR/rest-pr-list-case6.txt"
printf '603 factory/fake-bead-paginate\n' > "$GH_SHIM_REST_PR_LIST_OUT"
# PAGE1 (what a naive single-page-of-30 fetch would see): 30 clean runs, no
# bad run — would incorrectly read as green if pagination were not used.
GH_SHIM_CHECKRUNS_PAGE1_OUT="$SCRATCH_DIR/checkruns-case6-page1.txt"
: > "$GH_SHIM_CHECKRUNS_PAGE1_OUT"
i=1
while [ "$i" -le 30 ]; do printf 'completed success\n' >> "$GH_SHIM_CHECKRUNS_PAGE1_OUT"; i=$((i + 1)); done
# FULL (what --paginate actually retrieves): the same 30 clean runs PLUS one
# cancelled run that would only be visible on page 2.
GH_SHIM_CHECKRUNS_FULL_OUT="$SCRATCH_DIR/checkruns-case6-full.txt"
cat "$GH_SHIM_CHECKRUNS_PAGE1_OUT" > "$GH_SHIM_CHECKRUNS_FULL_OUT"
printf 'completed cancelled\n' >> "$GH_SHIM_CHECKRUNS_FULL_OUT"
GH_SHIM_CHECKRUNS_OUT="/dev/null"
GH_SHIM_COMMIT_STATUS_OUT="/dev/null"
GH_SHIM_HEAD_SHA="deadbeefcase6"
out6="$(run_guard)"
echo "$out6"
call_log6="$(cat "$GH_SHIM_LOG")"
assert_contains "case6: check-runs call used --paginate" "--paginate" "$call_log6"
assert_contains "case6: output shows CI not green (bad run caught via pagination)" "CI not green" "$out6"
assert_not_contains "case6: gh pr merge NEVER called (page-2 bad run)" "pr merge" "$call_log6"

echo
echo "=== TEST CASE 7: failing legacy commit-status (check-runs all green) must still block ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-calls-case7.log"; : > "$GH_SHIM_LOG"
GH_SHIM_RATE_LIMIT_JSON="$SCRATCH_DIR/rate-limit-case7.json"
cat > "$GH_SHIM_RATE_LIMIT_JSON" <<'EOF_RL7'
{"resources":{"core":{"limit":5000,"remaining":4000,"reset":9999999999},"graphql":{"limit":5000,"remaining":50,"reset":9999999999}}}
EOF_RL7
GH_SHIM_REST_PR_LIST_OUT="$SCRATCH_DIR/rest-pr-list-case7.txt"
printf '604 factory/fake-bead-legacystatus\n' > "$GH_SHIM_REST_PR_LIST_OUT"
GH_SHIM_CHECKRUNS_OUT="$SCRATCH_DIR/checkruns-case7.txt"
printf 'completed success\n' > "$GH_SHIM_CHECKRUNS_OUT"
GH_SHIM_CHECKRUNS_FULL_OUT=""
GH_SHIM_CHECKRUNS_PAGE1_OUT=""
GH_SHIM_COMMIT_STATUS_OUT="$SCRATCH_DIR/commitstatus-case7.json"
printf '{"state":"failure","total_count":2}' > "$GH_SHIM_COMMIT_STATUS_OUT"
GH_SHIM_HEAD_SHA="deadbeefcase7"
out7="$(run_guard)"
echo "$out7"
assert_contains "case7: output shows CI not green for failing legacy status" "CI not green" "$out7"
assert_not_contains "case7: gh pr merge NEVER called (legacy status failure)" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
