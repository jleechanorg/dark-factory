#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
JOB="$ROOT/jobs/jleechanorg-pr-green/jleechanorg-pr-green-daily.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
mock_bin="$fixture_dir/bin"
codex_home="$fixture_dir/codex"
mkdir -p "$mock_bin" "$codex_home"
printf '%s\n' '{"tokens":{}}' >"$codex_home/auth.json"

cat >"$mock_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "api" ]]; then
  printf '%s\n' '[{"total_count":1,"incomplete_results":false,"items":[{"repository_url":"https://api.github.com/repos/jleechanorg/worldarchitect.ai","number":9941,"title":"prompt integration","html_url":"https://github.com/jleechanorg/worldarchitect.ai/pull/9941","updated_at":"2099-01-01T00:00:00Z","draft":false}]}]'
elif [[ "$1" == "pr" && "$2" == "view" ]]; then
  printf '%s\n' '{"headRefOid":"head-before","mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","statusCheckRollup":[]}'
else
  printf 'unexpected gh invocation: %s\n' "$*" >&2
  exit 1
fi
EOF

cat >"$mock_bin/ao" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "session" && "$2" == "ls" ]]; then
  printf '%s\n' '{"data":[]}'
  exit 0
fi
if [[ "$1" == "project" && "$2" == "get" ]]; then
  printf '{"status":"ok","project":{"id":"worldarchitect.ai","config":{"defaultBranch":"main","env":{"CODEX_HOME":"%s","OTHER":"preserve"}}}}\n' "$CODEX_HOME"
  exit 0
fi
if [[ "$1" == "project" && "$2" == "set-config" ]]; then
  exit 0
fi
if [[ "$1" == "spawn" ]]; then
  prompt=''
  while (($#)); do
    if [[ "$1" == "--prompt" ]]; then
      shift
      prompt="${1-}"
    fi
    shift
  done
  printf '%s' "$prompt" >"$AO_PROMPT_CAPTURE"
  sleep 0.5
  exit 0
fi
printf 'unexpected ao invocation: %s\n' "$*" >&2
exit 1
EOF
chmod +x "$mock_bin/gh" "$mock_bin/ao"

capture="$fixture_dir/prompt.txt"
PATH="$mock_bin:$PATH" \
  HOME="$fixture_dir/home" \
  CODEX_HOME="$codex_home" \
  PR_GREEN_METRICS_DIR="$fixture_dir/metrics" \
  PR_GREEN_AO_CONFIG_PATH="$fixture_dir/ao-config" \
  PR_GREEN_SPAWN_PROBE_SECONDS=0.1 \
  AO_PROMPT_CAPTURE="$capture" \
  bash "$JOB" >/dev/null

[[ -s "$capture" ]] || { echo 'FAIL: scheduler did not emit an AO prompt' >&2; exit 1; }

assert_prompt_contains() {
  local expected="$1"
  grep -Fq -- "$expected" "$capture" || {
    printf 'FAIL: AO prompt missing: %s\n' "$expected" >&2
    exit 1
  }
}

assert_prompt_does_not_contain() {
  local forbidden="$1"
  if grep -Fq -- "$forbidden" "$capture"; then
    printf 'FAIL: AO prompt retained premature-refusal language: %s\n' "$forbidden" >&2
    exit 1
  fi
}

assert_prompt_contains 'Work on https://github.com/jleechanorg/worldarchitect.ai/pull/9941 in worldarchitect.ai.'
assert_prompt_contains 'Distinguish textual Git conflicts, generated-file/checksum conflicts, post-merge test failures, and genuine product-policy disagreements.'
assert_prompt_contains 'A post-merge test failure is not automatically product ambiguity.'
assert_prompt_contains 'Preserve the PR user-visible behavior while adapting stale implementation and tests to the current base architecture.'
assert_prompt_contains 'A bounded integration repair may edit production code and tests together.'
assert_prompt_contains 'Regenerate derived manifests and checksums last.'
assert_prompt_contains 'Stop only when repository evidence leaves two or more genuinely plausible user-visible behaviors.'
assert_prompt_contains 'Never merge the PR.'
assert_prompt_contains 'Never rebase published history or rewrite history.'
assert_prompt_contains 'Never force-push.'
assert_prompt_contains 'Never change credentials.'
assert_prompt_contains 'Never weaken tests merely to make them pass.'
assert_prompt_contains 'Push normally only after the integrated tests and required checks are green.'
assert_prompt_does_not_contain 'Fix only easy, clearly scoped test failures or mechanical merge conflicts'
assert_prompt_does_not_contain 'leave it untouched and report the blocker'

printf 'jleechanorg-pr-green prompt integration: PASS\n'
