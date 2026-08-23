#!/usr/bin/env bash
# test_auto_merge_guard_repo_policy.sh — TDD coverage for the 2026-08-23
# PR-merge-storm incident remediation: auto-merge-guard.sh must HARD DENY
# jleechanorg/worldarchitect.ai regardless of any config, and fail-closed
# (refuse to merge) for any other repo not explicitly allowlisted via
# config/auto_merge_repo_allowlist.json.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GUARD="$ROOT/daemon/scripts/auto-merge-guard.sh"

PASS=0; FAIL=0
assert_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "PASS: $name"; PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected to find '$needle' in: $haystack)"; FAIL=$((FAIL + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-amg-repo-policy.XXXXXX)"
trap 'rm -rf "$SCRATCH_DIR"' EXIT

FAKE_BIN_DIR="$SCRATCH_DIR/bin"
mkdir -p "$FAKE_BIN_DIR"

# Fake gh: only needs to answer `gh repo view` and `gh api rate_limit` for
# this test — the script must exit before making any pr list/merge calls.
FAKE_GH="$FAKE_BIN_DIR/gh"
cat > "$FAKE_GH" <<'EOF_GH'
#!/usr/bin/env bash
set -u
: "${GH_SHIM_LOG:?GH_SHIM_LOG not set}"
printf '%s\n' "$*" >> "$GH_SHIM_LOG"
if [ "${1:-}" = "repo" ] && [ "${2:-}" = "view" ]; then
  echo "${GH_SHIM_REPO:?}"
  exit 0
fi
if [ "${1:-}" = "api" ] && [ "${2:-}" = "rate_limit" ]; then
  cat "${GH_SHIM_RATE_LIMIT_JSON:?}"
  exit 0
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "list" ]; then
  echo "GH_SHIM: unexpected pr list call — repo policy gate should have exited first" >&2
  exit 1
fi
if [ "${1:-}" = "pr" ] && [ "${2:-}" = "merge" ]; then
  echo "GH_SHIM: unexpected pr merge call — repo policy gate should have exited first" >&2
  exit 1
fi
echo "{}"; exit 0
EOF_GH
chmod +x "$FAKE_GH"

RATE_LIMIT_JSON="$SCRATCH_DIR/rate_limit.json"
cat > "$RATE_LIMIT_JSON" <<'EOF_RL'
{"resources":{"core":{"remaining":5000},"graphql":{"remaining":5000}}}
EOF_RL

run_guard() {
  local repo="$1" policy_file="$2"
  GH_SHIM_LOG="$SCRATCH_DIR/gh.log" \
  GH_SHIM_REPO="$repo" \
  GH_SHIM_RATE_LIMIT_JSON="$RATE_LIMIT_JSON" \
  AMG_REPO_POLICY_FILE="$policy_file" \
  PATH="$FAKE_BIN_DIR:$PATH" \
    bash "$GUARD" 2>&1
}

# --- Case 1: worldarchitect.ai is HARD DENIED even with an allowlist that includes it ---
: > "$SCRATCH_DIR/gh.log"
ALLOW_WORLDAI="$SCRATCH_DIR/allow_worldai.json"
cat > "$ALLOW_WORLDAI" <<'EOF_CFG'
{"allowed_repos": ["jleechanorg/worldarchitect.ai"]}
EOF_CFG
OUT1="$(run_guard "jleechanorg/worldarchitect.ai" "$ALLOW_WORLDAI")"
assert_contains "worldai hard-denied even when explicitly allowlisted" "HARD DENY" "$OUT1"
assert_contains "no pr list call happened for worldai" "" "$(grep -c 'pr list' "$SCRATCH_DIR/gh.log" || true)"

# --- Case 2: a repo not in the allowlist is refused (fail-closed) ---
: > "$SCRATCH_DIR/gh.log"
ALLOW_ONLY_DF="$SCRATCH_DIR/allow_df.json"
cat > "$ALLOW_ONLY_DF" <<'EOF_CFG2'
{"allowed_repos": ["jleechanorg/dark-factory"]}
EOF_CFG2
OUT2="$(run_guard "jleechanorg/some-other-repo" "$ALLOW_ONLY_DF")"
assert_contains "unlisted repo refused (fail-closed)" "not in the allowed_repos list" "$OUT2"

# --- Case 3: missing config file means refuse everything ---
OUT3="$(run_guard "jleechanorg/dark-factory" "$SCRATCH_DIR/does-not-exist.json")"
assert_contains "missing config file refuses (fail-closed default)" "no repo allowlist config" "$OUT3"

# --- Case 4: allowlisted repo proceeds past the policy gate (reaches pr list) ---
: > "$SCRATCH_DIR/gh.log"
OUT4="$(run_guard "jleechanorg/dark-factory" "$ALLOW_ONLY_DF" || true)"
if grep -q "pr list" "$SCRATCH_DIR/gh.log"; then
  echo "PASS: allowlisted repo proceeds past policy gate to pr list"; PASS=$((PASS + 1))
else
  echo "FAIL: allowlisted repo did not reach pr list (log: $(cat "$SCRATCH_DIR/gh.log"))"; FAIL=$((FAIL + 1))
fi

echo ""
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
