#!/usr/bin/env bash
# test_auto_merge_guard_human_approval_marker.sh — TDD coverage for bead
# rev-iwywa: a SECOND, independent merge gate alongside the repo-allowlist
# gate from PR #735.
#
# BACKGROUND: CLAUDE.md requires a literal "MERGE APPROVED" phrase from a
# human before any merge to worldarchitect.ai — but that policy was, until
# this bead, enforced by session-prompt convention ONLY. There was zero
# code-level check for a human-approval marker anywhere in
# auto-merge-guard.sh or gh-pr-merge-wrapper.sh. The moment a repo is
# re-added to config/auto_merge_repo_allowlist.json (a one-line JSON edit —
# that IS the point of PR #735's design), the exact unattended-merge gap
# that caused the 2026-08-23 incident reopens with zero additional
# protection, because the allowlist alone doesn't prove a human approved
# THAT SPECIFIC PR.
#
# This file proves:
#   1. A PR that passes every existing gate (CI green, assessment green,
#      mergeable, repo-allowlisted) but has NO approval marker is refused,
#      with the REFUSED:NO_APPROVAL_MARKER log line — merge never attempted.
#   2. A PR with a valid marker (human, non-author, non-bot "MERGE APPROVED"
#      comment) proceeds to merge — existing behavior preserved end-to-end
#      through the real gh-pr-merge-wrapper.sh.
#   3. Anti-spoofing: a "MERGE APPROVED" comment posted by the PR's OWN
#      author must NOT satisfy the gate.
#   4. Anti-spoofing: a "MERGE APPROVED" comment posted by a bot account
#      (type=Bot, login ending [bot]) must NOT satisfy the gate.
#   5. The OR-label path: "auto-merge-approved" applied by a human actor
#      approves; applied by a bot actor does not.
#
# Run with:
#   bash tests/scripts/test_auto_merge_guard_human_approval_marker.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"
PR_NUM=88001
LIVE_HEAD="cafebabecafebabecafebabecafebabecafebabe"

PASS=0; FAIL=0
assert_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected to find '$needle' in: $haystack)"; FAIL=$((FAIL + 1))
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

SCRATCH_DIR="$(mktemp -d -t test-amg-approval.XXXXXX)"
trap 'rm -rf "$SCRATCH_DIR"' EXIT

FAKE_BIN_DIR="$SCRATCH_DIR/bin"
mkdir -p "$FAKE_BIN_DIR"
FAKE_HOME="$SCRATCH_DIR/home"
mkdir -p "$FAKE_HOME"

FAKE_REPO_POLICY="$SCRATCH_DIR/repo_allowlist.json"
cat > "$FAKE_REPO_POLICY" <<'EOF_POLICY'
{"allowed_repos": ["jleechanorg/dark-factory"]}
EOF_POLICY

RATE_LIMIT_JSON="$SCRATCH_DIR/rate_limit.json"
cat > "$RATE_LIMIT_JSON" <<'EOF_RL'
{"resources":{"core":{"limit":5000,"remaining":4000,"reset":9999999999},"graphql":{"limit":5000,"remaining":4000,"reset":9999999999}}}
EOF_RL

PR_LIST_OUT="$SCRATCH_DIR/pr_list.txt"
printf '%d factory/fake-bead-r1\n' "$PR_NUM" > "$PR_LIST_OUT"

CHECKS_OUT="$SCRATCH_DIR/checks.txt"
printf 'pass\npass\n' > "$CHECKS_OUT"

# --- Fake gh shim -------------------------------------------------------
# Logs every invocation to GH_SHIM_LOG and returns canned output for the
# exact subcommands the guard AND the real gh-pr-merge-wrapper.sh issue —
# neither script ever hits the real GitHub API in this test.
FAKE_GH="$FAKE_BIN_DIR/gh"
cat > "$FAKE_GH" <<'EOF_GH'
#!/usr/bin/env bash
set -u
: "${GH_SHIM_LOG:?GH_SHIM_LOG not set}"
printf '%s\n' "$*" >> "$GH_SHIM_LOG"

if [ "${1:-}" = "repo" ] && [ "${2:-}" = "view" ]; then
  echo "jleechanorg/dark-factory"; exit 0
fi
if [ "${1:-}" = "api" ] && [ "${2:-}" = "rate_limit" ]; then
  cat "${GH_SHIM_RATE_LIMIT_JSON:?}"; exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "list" ]; then
  cat "${GH_SHIM_PR_LIST_OUT:-/dev/null}"; exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "checks" ]; then
  cat "${GH_SHIM_CHECKS_OUT:-/dev/null}"; exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "view" ]; then
  for a in "$@"; do
    case "$a" in
      *headRefOid*) echo "${GH_SHIM_HEAD_SHA:-deadbeef}"; exit 0 ;;
      *isDraft*) echo "${GH_SHIM_IS_DRAFT:-false}"; exit 0 ;;
      *mergeable*) echo "${GH_SHIM_MERGEABLE_GQL:-MERGEABLE}"; exit 0 ;;
      *state*) echo "${GH_SHIM_STATE_GQL:-MERGED}"; exit 0 ;;
    esac
  done
  echo "{}"; exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "merge" ]; then
  exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "ready" ]; then
  exit 0
fi
if [ "${1:-}" = "api" ]; then
  endpoint="${2:-}"
  case "$endpoint" in
    *"/comments"*)
      cat "${GH_SHIM_COMMENTS_OUT:-/dev/null}"; exit 0 ;;
    *"/events"*)
      cat "${GH_SHIM_EVENTS_OUT:-/dev/null}"; exit 0 ;;
    *"/pulls/"*)
      for a in "$@"; do
        case "$a" in
          *user.login*) echo "${GH_SHIM_AUTHOR_LOGIN:-pr-author}"; exit 0 ;;
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

emit_assessment() {
  local log="$1" head="${2:-$LIVE_HEAD}"
  local gates='{"ci_green":"pass","no_conflicts":"pass","coderabbit":"pass","bugbot":"pass","comments_resolved":"pass","evidence_review":"pass","skeptic":"pass","vacuous_red_green":"pass"}'
  printf '{"timestamp":"2026-08-23T00:00:00Z","eventType":"GATE_ASSESSMENT","bead_id":"fake-bead","attempt":1,"state":"ATTESTED","context":{"pr_number":%d,"gates":%s,"all_green":true,"head_sha":"%s"}}\n' \
    "$PR_NUM" "$gates" "$head" > "$log"
}

run_guard() {
  local afd_log="$SCRATCH_DIR/daemon.jsonl"
  emit_assessment "$afd_log"
  ( cd "$ROOT" && \
    HOME="$FAKE_HOME" \
    PATH="$FAKE_BIN_DIR:$PATH" \
    AFD_LOG="$afd_log" \
    AMG_REPO_POLICY_FILE="$FAKE_REPO_POLICY" \
    GH_SHIM_LOG="$GH_SHIM_LOG" \
    GH_SHIM_RATE_LIMIT_JSON="$RATE_LIMIT_JSON" \
    GH_SHIM_PR_LIST_OUT="$PR_LIST_OUT" \
    GH_SHIM_CHECKS_OUT="$CHECKS_OUT" \
    GH_SHIM_HEAD_SHA="$LIVE_HEAD" \
    GH_SHIM_AUTHOR_LOGIN="${GH_SHIM_AUTHOR_LOGIN:-pr-author}" \
    GH_SHIM_COMMENTS_OUT="${GH_SHIM_COMMENTS_OUT:-/dev/null}" \
    GH_SHIM_EVENTS_OUT="${GH_SHIM_EVENTS_OUT:-/dev/null}" \
    bash "$GUARD" 2>&1 )
}

echo "=== CASE 1: no approval marker at all -> REFUSED, merge never attempted ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-case1.log"; : > "$GH_SHIM_LOG"
GH_SHIM_AUTHOR_LOGIN="pr-author" GH_SHIM_COMMENTS_OUT="/dev/null" GH_SHIM_EVENTS_OUT="/dev/null" \
  out1="$(run_guard)"
echo "$out1"
assert_contains "case1: REFUSED:NO_APPROVAL_MARKER logged" "REFUSED:NO_APPROVAL_MARKER" "$out1"
assert_not_contains "case1: gh pr merge never called" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== CASE 2: valid human, non-author, non-bot comment -> merges (existing behavior preserved) ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-case2.log"; : > "$GH_SHIM_LOG"
COMMENTS2="$SCRATCH_DIR/comments2.json"
cat > "$COMMENTS2" <<'EOF_C2'
[{"body":"looks good\nMERGE APPROVED","user":{"login":"jleechan2015","type":"User"}}]
EOF_C2
GH_SHIM_AUTHOR_LOGIN="some-factory-bot-author" GH_SHIM_COMMENTS_OUT="$COMMENTS2" GH_SHIM_EVENTS_OUT="/dev/null" \
  out2="$(run_guard)"
echo "$out2"
assert_not_contains "case2: no REFUSED:NO_APPROVAL_MARKER" "REFUSED:NO_APPROVAL_MARKER" "$out2"
assert_contains "case2: gh pr merge WAS called (merge proceeded)" "pr merge" "$(cat "$GH_SHIM_LOG")"
assert_contains "case2: guard reports MERGED" "MERGED" "$out2"

echo
echo "=== CASE 3 (anti-spoof): PR's own author posting MERGE APPROVED -> REFUSED ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-case3.log"; : > "$GH_SHIM_LOG"
COMMENTS3="$SCRATCH_DIR/comments3.json"
cat > "$COMMENTS3" <<'EOF_C3'
[{"body":"MERGE APPROVED","user":{"login":"pr-self-author","type":"User"}}]
EOF_C3
GH_SHIM_AUTHOR_LOGIN="pr-self-author" GH_SHIM_COMMENTS_OUT="$COMMENTS3" GH_SHIM_EVENTS_OUT="/dev/null" \
  out3="$(run_guard)"
echo "$out3"
assert_contains "case3: author self-approval REFUSED:NO_APPROVAL_MARKER" "REFUSED:NO_APPROVAL_MARKER" "$out3"
assert_not_contains "case3: gh pr merge never called (self-approval spoof blocked)" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== CASE 4 (anti-spoof): bot account posting MERGE APPROVED -> REFUSED ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-case4.log"; : > "$GH_SHIM_LOG"
COMMENTS4="$SCRATCH_DIR/comments4.json"
cat > "$COMMENTS4" <<'EOF_C4'
[{"body":"MERGE APPROVED","user":{"login":"github-actions[bot]","type":"Bot"}},
 {"body":"MERGE APPROVED","user":{"login":"some-ci-bot","type":"User"}}]
EOF_C4
GH_SHIM_AUTHOR_LOGIN="pr-author" GH_SHIM_COMMENTS_OUT="$COMMENTS4" GH_SHIM_EVENTS_OUT="/dev/null" \
  out4="$(run_guard)"
echo "$out4"
assert_contains "case4: bot-account approval REFUSED:NO_APPROVAL_MARKER" "REFUSED:NO_APPROVAL_MARKER" "$out4"
assert_not_contains "case4: gh pr merge never called (bot spoof blocked)" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== CASE 5a: 'auto-merge-approved' label applied by a HUMAN actor -> merges ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-case5a.log"; : > "$GH_SHIM_LOG"
EVENTS5A="$SCRATCH_DIR/events5a.json"
cat > "$EVENTS5A" <<'EOF_E5A'
[{"event":"labeled","label":{"name":"auto-merge-approved"},"actor":{"login":"jleechan2015","type":"User"}}]
EOF_E5A
GH_SHIM_AUTHOR_LOGIN="pr-author" GH_SHIM_COMMENTS_OUT="/dev/null" GH_SHIM_EVENTS_OUT="$EVENTS5A" \
  out5a="$(run_guard)"
echo "$out5a"
assert_not_contains "case5a: no REFUSED:NO_APPROVAL_MARKER" "REFUSED:NO_APPROVAL_MARKER" "$out5a"
assert_contains "case5a: gh pr merge WAS called (label path approved)" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== CASE 5b: 'auto-merge-approved' label applied by a BOT actor -> REFUSED ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-case5b.log"; : > "$GH_SHIM_LOG"
EVENTS5B="$SCRATCH_DIR/events5b.json"
cat > "$EVENTS5B" <<'EOF_E5B'
[{"event":"labeled","label":{"name":"auto-merge-approved"},"actor":{"login":"dependabot[bot]","type":"Bot"}}]
EOF_E5B
GH_SHIM_AUTHOR_LOGIN="pr-author" GH_SHIM_COMMENTS_OUT="/dev/null" GH_SHIM_EVENTS_OUT="$EVENTS5B" \
  out5b="$(run_guard)"
echo "$out5b"
assert_contains "case5b: bot-applied label REFUSED:NO_APPROVAL_MARKER" "REFUSED:NO_APPROVAL_MARKER" "$out5b"
assert_not_contains "case5b: gh pr merge never called (bot-label spoof blocked)" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== CASE 6 (anti-spoof): negated mention of the phrase -> REFUSED, not a false-positive match ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-case6.log"; : > "$GH_SHIM_LOG"
COMMENTS6="$SCRATCH_DIR/comments6.json"
cat > "$COMMENTS6" <<'EOF_C6'
[{"body":"I do NOT think this should say MERGE APPROVED yet -- needs more review first.","user":{"login":"jleechan2015","type":"User"}}]
EOF_C6
GH_SHIM_AUTHOR_LOGIN="some-factory-bot-author" GH_SHIM_COMMENTS_OUT="$COMMENTS6" GH_SHIM_EVENTS_OUT="/dev/null" \
  out6="$(run_guard)"
echo "$out6"
assert_contains "case6: negated mention REFUSED:NO_APPROVAL_MARKER (substring match must not satisfy the gate)" "REFUSED:NO_APPROVAL_MARKER" "$out6"
assert_not_contains "case6: gh pr merge never called (negation spoof blocked)" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo
echo "=== CASE 7 (anti-spoof): phrase embedded mid-sentence / inside a quote -> REFUSED ==="
GH_SHIM_LOG="$SCRATCH_DIR/gh-case7.log"; : > "$GH_SHIM_LOG"
COMMENTS7="$SCRATCH_DIR/comments7.json"
cat > "$COMMENTS7" <<'EOF_C7'
[{"body":"Quoting the policy doc: \"...requires a literal MERGE APPROVED comment before...\" -- not doing that here, just referencing it.","user":{"login":"jleechan2015","type":"User"}}]
EOF_C7
GH_SHIM_AUTHOR_LOGIN="some-factory-bot-author" GH_SHIM_COMMENTS_OUT="$COMMENTS7" GH_SHIM_EVENTS_OUT="/dev/null" \
  out7="$(run_guard)"
echo "$out7"
assert_contains "case7: mid-sentence/quoted mention REFUSED:NO_APPROVAL_MARKER (must be a standalone line)" "REFUSED:NO_APPROVAL_MARKER" "$out7"
assert_not_contains "case7: gh pr merge never called (embedded-mention spoof blocked)" "pr merge" "$(cat "$GH_SHIM_LOG")"

echo ""
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
