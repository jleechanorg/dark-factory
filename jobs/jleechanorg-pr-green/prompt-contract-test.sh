#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
PROMPT_SOURCE="$SCRIPT_DIR/jleechanorg-pr-green-daily.sh"

assert_prompt_contains() {
  local expected="$1"
  if ! grep -Fq -- "$expected" "$PROMPT_SOURCE"; then
    printf 'prompt contract missing: %s\n' "$expected" >&2
    exit 1
  fi
}

assert_prompt_does_not_contain() {
  local forbidden="$1"
  if grep -Fq -- "$forbidden" "$PROMPT_SOURCE"; then
    printf 'prompt contract retained premature-refusal language: %s\n' "$forbidden" >&2
    exit 1
  fi
}

# The worker must classify failures before deciding whether to stop. A
# post-merge test failure can be an integration mismatch, not an unresolved
# product decision.
assert_prompt_contains 'Distinguish textual Git conflicts, generated-file/checksum conflicts, post-merge test failures, and genuine product-policy disagreements.'
assert_prompt_contains 'A post-merge test failure is not automatically product ambiguity.'
assert_prompt_contains 'Preserve the PR user-visible behavior while adapting stale implementation and tests to the current base architecture.'
assert_prompt_contains 'A bounded integration repair may edit production code and tests together.'
assert_prompt_contains 'Regenerate derived manifests and checksums last.'
assert_prompt_contains 'Stop only when repository evidence leaves two or more genuinely plausible user-visible behaviors.'
assert_prompt_contains 'Push normally only after the integrated tests and required checks are green.'
assert_prompt_contains 'Never manufacture an empty or no-op commit, bypass hooks, or wrap/replace push tools to obtain a receipt or satisfy delivery metrics.'
assert_prompt_contains 'If the push succeeds but receipt/verification remains pending or unavailable, preserve the exact before/after SHAs and logs, do not create another commit or push solely to obtain a metric/receipt, and report verification pending.'
assert_prompt_does_not_contain 'Fix only easy, clearly scoped test failures or mechanical merge conflicts'
assert_prompt_does_not_contain 'If the issue is ambiguous, risky, or not mechanically solvable, leave it untouched'

printf 'prompt contract: PASS\n'
