#!/usr/bin/env bash
# test_auto_merge_guard_bash_n_clean.sh — regression test for a bash -n
# parse failure that snuck into daemon/scripts/auto-merge-guard.sh on
# PR #421 r4 (commit 3444726). The python predicate inside the
# `latest_assessment_no_red` heredoc is delimited by single quotes, but a
# comment line inside the python contained an unescaped apostrophe
# (`factory-overlay.sh`'s REQUIRED_KEYS) which closes the bash heredoc
# early and breaks `bash -n` tokenization.
#
# CI's `test` job runs `bash -n daemon/scripts/auto-merge-guard.sh`
# before exercising the scheduler; that gate failed on PR #421 r4 and
# was the visible cause of the run's red `test` check. This test pins
# both the parse-clean invariant and the absence of unescaped single
# quotes inside the python heredoc, so the failure mode cannot recur
# silently.
set -uo pipefail

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

# 1. bash -n must accept the file.
if bash -n "$GUARD" 2>/tmp/bash-n.err; then
  echo "PASS: bash -n parses auto-merge-guard.sh"; PASS=$((PASS + 1))
else
  echo "FAIL: bash -n rejects auto-merge-guard.sh: $(cat /tmp/bash-n.err)"
  FAIL=$((FAIL + 1))
fi

# 2. The python heredoc MUST be a single quoted string to bash, so any
# unescaped single quote inside the heredoc body would close it early.
# Locate the heredoc by finding `python3 -c '` on the open line and the
# `' "$live_head"` closer. Anything strictly between them must contain
# zero apostrophes — only the open and close lines should have them.
heredoc_open="$(grep -nF "python3 -c '" "$GUARD" | head -1 | cut -d: -f1)"
heredoc_close="$(grep -nF "' \"\$live_head\"" "$GUARD" | head -1 | cut -d: -f1)"
if [ -z "$heredoc_open" ] || [ -z "$heredoc_close" ]; then
  echo "FAIL: could not locate python3 -c heredoc bounds"; FAIL=$((FAIL + 1))
else
  body="$(awk -v s="$heredoc_open" -v e="$heredoc_close" 'NR>s && NR<e' "$GUARD")"
  quote_count="$(printf '%s' "$body" | tr -cd "'" | wc -c | tr -d ' ')"
  assert "python heredoc body has zero unescaped apostrophes (only open+close hold quotes)" \
    "0" "$quote_count"
fi

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0