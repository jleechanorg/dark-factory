#!/usr/bin/env bash
# test_gh_pr_merge_wrapper_provenance.sh — TDD coverage for bead rev-377j4:
# the 2026-08-23 PR-merge-storm postmortem took 1+ hour of cross-host
# detective work to establish which host/script performed a batch of
# unattended merges, because gh-pr-merge-wrapper.sh (the ONE place that
# actually invokes `gh pr merge` — see test_auto_merge_guard_uses_merge_
# wrapper.sh) only ever wrote human-readable `echo` lines to stdout, and
# no telemetry event anywhere carried host/process identity.
#
# Uses the same fake-`gh`-on-PATH interception pattern as
# test_auto_merge_guard_quota_routing.sh's FAKE_GH, and proves:
#   1. a merge invocation appends exactly one structured JSONL line to
#      $AFD_LOG
#   2. that line is valid JSON with ts, hostname, script_path,
#      repo_git_sha_at_invocation, pr_number, triggering_mechanism
#   3. the line lands BEFORE the actual `gh pr merge` call — proved by
#      having the fake `gh pr merge` handler itself assert AFD_LOG already
#      contains this PR's line at the moment it runs (no race: the real
#      script writes the log synchronously, then `exec`s gh, so ordering
#      is process-sequential, not concurrent)
#   4. triggering_mechanism defaults to "manual-wrapper-invocation" for a
#      direct/manual invocation, and reflects $AMG_TRIGGERING_MECHANISM
#      when set (exactly how auto-merge-guard.sh invokes this wrapper)
#   5. auto-merge-guard.sh's merge-path line sets
#      AMG_TRIGGERING_MECHANISM="auto-merge-guard.sh" (static check)
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WRAPPER="$ROOT/daemon/scripts/gh-pr-merge-wrapper.sh"
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
  # Avoid `printf | grep -q` under `pipefail`: once grep finds a match it
  # closes the pipe, and a large haystack can make printf exit on SIGPIPE,
  # turning a true match into a flaky failure.
  if [[ "$haystack" == *"$needle"* ]]; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected to find '$needle' in: $haystack)"; FAIL=$((FAIL + 1))
  fi
}

[ -f "$WRAPPER" ] || { echo "FAIL: wrapper script missing at $WRAPPER"; exit 1; }
[ -f "$GUARD" ] || { echo "FAIL: guard script missing at $GUARD"; exit 1; }

SCRATCH_DIR="$(mktemp -d -t test-amg-provenance.XXXXXX)"
trap 'rm -rf "$SCRATCH_DIR"' EXIT

FAKE_BIN_DIR="$SCRATCH_DIR/bin"
mkdir -p "$FAKE_BIN_DIR"

# --- Fake gh shim: handles exactly the subcommands gh-pr-merge-wrapper.sh
# issues. `gh pr merge` is the ordering-proof point: before doing anything
# else it checks AFD_LOG already mentions this PR's pr_number key; if not,
# it drops an ORDER_VIOLATION marker the test asserts is absent.
FAKE_GH="$FAKE_BIN_DIR/gh"
cat > "$FAKE_GH" <<'EOF_GH'
#!/usr/bin/env bash
set -u
if [ "${1:-}" = "repo" ] && [ "${2:-}" = "view" ]; then
  echo "jleechanorg/dark-factory"
  exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "view" ]; then
  echo "false"
  exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "merge" ]; then
  pr="${3:-}"
  if [ -n "${AFD_LOG:-}" ] && [ -f "${AFD_LOG:-}" ] && grep -qF "\"pr_number\": $pr" "$AFD_LOG"; then
    :
  else
    echo "ORDER_VIOLATION: provenance line missing before gh pr merge for PR $pr" > "${ORDER_FAIL_MARKER:?}"
  fi
  echo "merged $pr"
  exit 0
fi
echo "unhandled fake gh invocation: $*" >&2
exit 1
EOF_GH
chmod +x "$FAKE_GH"

AFD_LOG="$SCRATCH_DIR/daemon.jsonl"
ORDER_FAIL_MARKER="$SCRATCH_DIR/order_fail"

# === Case 1: direct/manual invocation ===================================
rm -f "$AFD_LOG" "$ORDER_FAIL_MARKER"
case1_out="$(PATH="$FAKE_BIN_DIR:$PATH" AFD_LOG="$AFD_LOG" ORDER_FAIL_MARKER="$ORDER_FAIL_MARKER" \
  GH_REPO="jleechanorg/dark-factory" \
  bash "$WRAPPER" 4242 --squash 2>&1)"
case1_rc=$?
assert "case1: wrapper exits 0" "0" "$case1_rc"

if [ -f "$ORDER_FAIL_MARKER" ]; then
  echo "FAIL: case1: $(cat "$ORDER_FAIL_MARKER")"; FAIL=$((FAIL + 1))
else
  echo "PASS: case1: provenance line written before gh pr merge"; PASS=$((PASS + 1))
fi

[ -f "$AFD_LOG" ] || { echo "FAIL: case1: AFD_LOG was never created"; FAIL=$((FAIL + 1)); }
line_count="$(grep -c . "$AFD_LOG" 2>/dev/null || echo 0)"
assert "case1: exactly one JSONL line written" "1" "$line_count"

line1="$(cat "$AFD_LOG" 2>/dev/null)"
parsed1="$(printf '%s' "$line1" | python3 -c '
import json, sys
try:
    d = json.loads(sys.stdin.read())
except Exception as e:
    print("PARSE_ERROR:" + str(e))
    raise SystemExit(0)
required = ["ts", "hostname", "script_path", "repo_git_sha_at_invocation", "pr_number", "triggering_mechanism"]
missing = [k for k in required if k not in d]
if missing:
    print("MISSING:" + ",".join(missing))
else:
    print("OK pr_number=%s mechanism=%s script_path=%s hostname=%s" % (
        d["pr_number"], d["triggering_mechanism"], d["script_path"], d["hostname"]
    ))
')"
assert_contains "case1: JSONL line has all 6 required fields" "OK " "$parsed1"
assert_contains "case1: pr_number is 4242 (as JSON int)" "pr_number=4242" "$parsed1"
assert_contains "case1: default triggering_mechanism is manual-wrapper-invocation" \
  "mechanism=manual-wrapper-invocation" "$parsed1"
assert_contains "case1: script_path identifies gh-pr-merge-wrapper.sh" \
  "gh-pr-merge-wrapper.sh" "$parsed1"
assert_contains "case1: hostname is non-empty" "hostname=" "$parsed1"

# === Case 2: invoked exactly as auto-merge-guard.sh invokes it ==========
rm -f "$AFD_LOG" "$ORDER_FAIL_MARKER"
case2_out="$(PATH="$FAKE_BIN_DIR:$PATH" AFD_LOG="$AFD_LOG" ORDER_FAIL_MARKER="$ORDER_FAIL_MARKER" \
  AMG_TRIGGERING_MECHANISM="auto-merge-guard.sh" GH_REPO="jleechanorg/dark-factory" \
  bash "$WRAPPER" 5150 --squash 2>&1)"
case2_rc=$?
assert "case2: wrapper exits 0" "0" "$case2_rc"

if [ -f "$ORDER_FAIL_MARKER" ]; then
  echo "FAIL: case2: $(cat "$ORDER_FAIL_MARKER")"; FAIL=$((FAIL + 1))
else
  echo "PASS: case2: provenance line written before gh pr merge"; PASS=$((PASS + 1))
fi

line2="$(cat "$AFD_LOG" 2>/dev/null)"
assert_contains "case2: triggering_mechanism attributed to auto-merge-guard.sh" \
  '"triggering_mechanism": "auto-merge-guard.sh"' "$line2"
assert_contains "case2: pr_number is 5150 (as JSON int)" '"pr_number": 5150' "$line2"

real_head_sha="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo "")"
if [ -n "$real_head_sha" ]; then
  assert_contains "case2: repo_git_sha_at_invocation matches real repo HEAD" "$real_head_sha" "$line2"
fi

# === Static: auto-merge-guard.sh attributes the mechanism on its merge path
GUARD_CONTENT="$(cat "$GUARD")"
assert_contains "auto-merge-guard.sh sets AMG_TRIGGERING_MECHANISM=\"auto-merge-guard.sh\" before invoking the wrapper" \
  'AMG_TRIGGERING_MECHANISM="auto-merge-guard.sh"' "$GUARD_CONTENT"
assert_contains "auto-merge-guard.sh still routes merges through gh-pr-merge-wrapper.sh (not raw gh pr merge)" \
  "gh-pr-merge-wrapper.sh" "$GUARD_CONTENT"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
