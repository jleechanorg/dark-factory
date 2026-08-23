#!/usr/bin/env bash
# test_gh_pr_merge_wrapper.sh — behavioral tests for
# daemon/scripts/gh-pr-merge-wrapper.sh (bead rev-pkojz).
#
# ROOT-CAUSE under test: `gh pr merge <PR> --admin --squash` fails with a
# GraphQL "Pull Request is still a draft (mergePullRequest)" error when the
# PR is in draft state. The wrapper must auto-promote via `gh pr ready`
# first, merge unchanged when the PR is already non-draft, and log what it
# did either way.
#
# Strategy: put a fake `gh` binary on PATH ahead of the real one. The fake
# records every invocation (one line per call, args joined by `|`) to
# CALL_LOG and answers `pr view ... isDraft` from the DRAFT_STATE env var,
# so the test proves the wrapper's real branching logic — not a paraphrase
# of it — without touching the network.
#
# Run with: bash tests/scripts/test_gh_pr_merge_wrapper.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WRAPPER="$ROOT/daemon/scripts/gh-pr-merge-wrapper.sh"

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
  local name="$1" haystack="$2" needle="$3"
  case "$haystack" in
    *"$needle"*) echo "PASS: $name"; PASS=$((PASS + 1)) ;;
    *) echo "FAIL: $name (expected to find '$needle')"; FAIL=$((FAIL + 1)) ;;
  esac
}
assert_not_contains() {
  local name="$1" haystack="$2" needle="$3"
  case "$haystack" in
    *"$needle"*) echo "FAIL: $name (unexpectedly found '$needle')"; FAIL=$((FAIL + 1)) ;;
    *) echo "PASS: $name"; PASS=$((PASS + 1)) ;;
  esac
}

FAKE_BIN_DIR="$(mktemp -d -t gh-pr-merge-wrapper-test.XXXXXX)"
CALL_LOG="$FAKE_BIN_DIR/calls.log"
trap 'rm -rf "$FAKE_BIN_DIR"' EXIT

cat > "$FAKE_BIN_DIR/gh" <<'FAKE_GH_EOF'
#!/usr/bin/env bash
# Fake `gh` for gh-pr-merge-wrapper tests. Records every call, answers
# `pr view --json isDraft` from DRAFT_STATE, and `pr merge` exits with
# MERGE_EXIT_CODE (default 0) after printing a synthetic merge line.
echo "$*" >> "$CALL_LOG"
if [ "$1" = "repo" ] && [ "$2" = "view" ]; then
  echo "testorg/testrepo"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  echo "${DRAFT_STATE:-false}"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "ready" ]; then
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "merge" ]; then
  echo "Merged pull request"
  exit "${MERGE_EXIT_CODE:-0}"
fi
echo "fake gh: unhandled invocation: $*" >&2
exit 99
FAKE_GH_EOF
chmod +x "$FAKE_BIN_DIR/gh"
export CALL_LOG
export PATH="$FAKE_BIN_DIR:$PATH"

echo "=== gh-pr-merge-wrapper.sh ==="

# -- 0. bash -n clean --------------------------------------------------------
if bash -n "$WRAPPER"; then
  echo "PASS: wrapper bash -n clean"; PASS=$((PASS + 1))
else
  echo "FAIL: wrapper bash -n failed"; FAIL=$((FAIL + 1))
fi

# -- 1. draft PR: promotes via `gh pr ready` before merging ------------------
: > "$CALL_LOG"
DRAFT_STATE=true out="$(DRAFT_STATE=true "$WRAPPER" 9100 --admin --squash 2>&1)"
rc=$?
assert "draft PR: wrapper exits 0 (merge succeeded)" "0" "$rc"
calls="$(cat "$CALL_LOG")"
assert_contains "draft PR: calls 'pr ready 9100'" "$calls" "pr ready 9100"
assert_contains "draft PR: calls 'pr merge 9100 --repo testorg/testrepo --admin --squash'" "$calls" "pr merge 9100 --repo testorg/testrepo --admin --squash"
assert_contains "draft PR: logs promotion action" "$out" "promoting via 'gh pr ready 9100'"
# 'pr ready' must run strictly before 'pr merge' in the call order
ready_line="$(grep -n '^pr ready 9100' "$CALL_LOG" | head -1 | cut -d: -f1)"
merge_line="$(grep -n '^pr merge 9100' "$CALL_LOG" | head -1 | cut -d: -f1)"
if [ -n "$ready_line" ] && [ -n "$merge_line" ] && [ "$ready_line" -lt "$merge_line" ]; then
  echo "PASS: draft PR: 'pr ready' runs before 'pr merge'"; PASS=$((PASS + 1))
else
  echo "FAIL: draft PR: 'pr ready' did not run before 'pr merge' (ready_line=$ready_line merge_line=$merge_line)"; FAIL=$((FAIL + 1))
fi

# -- 2. non-draft PR: merges normally, no extra `gh pr ready` call -----------
: > "$CALL_LOG"
out="$(DRAFT_STATE=false "$WRAPPER" 9200 --admin --squash 2>&1)"
rc=$?
assert "non-draft PR: wrapper exits 0" "0" "$rc"
calls="$(cat "$CALL_LOG")"
assert_not_contains "non-draft PR: never calls 'pr ready'" "$calls" "pr ready"
assert_contains "non-draft PR: calls 'pr merge 9200 --repo testorg/testrepo --admin --squash'" "$calls" "pr merge 9200 --repo testorg/testrepo --admin --squash"
assert_contains "non-draft PR: logs no-promotion-needed" "$out" "is not a draft — no promotion needed"

# -- 3. underlying gh pr merge failure propagates as wrapper exit code -------
: > "$CALL_LOG"
DRAFT_STATE=false MERGE_EXIT_CODE=7 "$WRAPPER" 9300 --admin --squash >/dev/null 2>&1
rc=$?
assert "merge failure: wrapper propagates gh's exit code" "7" "$rc"

# -- 4. missing PR argument -> usage error, exit 2 ---------------------------
: > "$CALL_LOG"
out="$("$WRAPPER" 2>&1)"
rc=$?
assert "no PR arg: exits 2" "2" "$rc"
assert_contains "no PR arg: prints usage" "$out" "usage:"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
