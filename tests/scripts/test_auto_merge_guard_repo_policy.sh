#!/usr/bin/env bash
# test_auto_merge_guard_repo_policy.sh — TDD coverage for the 2026-08-23
# PR-merge-storm incident remediation: which repos auto-merge-guard.sh may
# auto-merge is controlled ENTIRELY by config/auto_merge_repo_allowlist.json
# (or the AMG_REPO_POLICY_FILE override) — no repo name is hardcoded in the
# script. Operator directive: keep the factory dispatch daemon running,
# stop only the merges, and make stop/resume a config edit, never a code
# change or redeploy.
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
assert_not_contains() {
  local name="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "FAIL: $name (unexpected: found '$needle')"; FAIL=$((FAIL + 1))
  else
    echo "PASS: $name"; PASS=$((PASS + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-amg-repo-policy.XXXXXX)"
trap 'rm -rf "$SCRATCH_DIR"' EXIT

FAKE_BIN_DIR="$SCRATCH_DIR/bin"
mkdir -p "$FAKE_BIN_DIR"

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

# --- Case 1: no repo name is hardcoded in the script ---
assert_not_contains "no repo name is hardcoded in the script" \
  'jleechanorg/worldarchitect.ai' "$(cat "$GUARD")"

# --- Case 2: worldai is refused when the config is empty (safe default) ---
: > "$SCRATCH_DIR/gh.log"
EMPTY_ALLOW="$SCRATCH_DIR/empty.json"
cat > "$EMPTY_ALLOW" <<'EOF_CFG'
{"allowed_repos": []}
EOF_CFG
OUT2="$(run_guard "jleechanorg/worldarchitect.ai" "$EMPTY_ALLOW")"
assert_contains "worldai refused with empty allowlist" "not in the allowed_repos list" "$OUT2"

# --- Case 3: worldai proceeds IF an operator explicitly adds it to config (config controls, not code) ---
: > "$SCRATCH_DIR/gh.log"
ALLOW_WORLDAI="$SCRATCH_DIR/allow_worldai.json"
cat > "$ALLOW_WORLDAI" <<'EOF_CFG2'
{"allowed_repos": ["jleechanorg/worldarchitect.ai"]}
EOF_CFG2
OUT3="$(run_guard "jleechanorg/worldarchitect.ai" "$ALLOW_WORLDAI" || true)"
if grep -q "pr list" "$SCRATCH_DIR/gh.log"; then
  echo "PASS: explicitly-allowlisted repo (even worldai) proceeds — control is config-only, not code"; PASS=$((PASS + 1))
else
  echo "FAIL: explicitly-allowlisted repo did not proceed (log: $(cat "$SCRATCH_DIR/gh.log"))"; FAIL=$((FAIL + 1))
fi

# --- Case 4: a repo not in the allowlist is refused (fail-closed) ---
: > "$SCRATCH_DIR/gh.log"
ALLOW_ONLY_DF="$SCRATCH_DIR/allow_df.json"
cat > "$ALLOW_ONLY_DF" <<'EOF_CFG3'
{"allowed_repos": ["jleechanorg/dark-factory"]}
EOF_CFG3
OUT4="$(run_guard "jleechanorg/some-other-repo" "$ALLOW_ONLY_DF")"
assert_contains "unlisted repo refused (fail-closed)" "not in the allowed_repos list" "$OUT4"

# --- Case 5: missing config file means refuse everything ---
OUT5="$(run_guard "jleechanorg/dark-factory" "$SCRATCH_DIR/does-not-exist.json")"
assert_contains "missing config file refuses (fail-closed default)" "no repo allowlist config" "$OUT5"

# --- Case 6: repo default shipped in the repo's own config is empty (verify the actual file, not a fixture) ---
DEFAULT_CFG="$ROOT/config/auto_merge_repo_allowlist.json"
[ -f "$DEFAULT_CFG" ] || { echo "FAIL: default config file missing at $DEFAULT_CFG"; FAIL=$((FAIL + 1)); }
DEFAULT_ALLOWED="$(python3 -c "import json; print(json.load(open('$DEFAULT_CFG')).get('allowed_repos', ['NONEMPTY']))" 2>/dev/null)"
if [ "$DEFAULT_ALLOWED" = "[]" ]; then
  echo "PASS: shipped default config has an empty allowlist (all auto-merging off by default)"; PASS=$((PASS + 1))
else
  echo "FAIL: shipped default config allowed_repos is not empty: $DEFAULT_ALLOWED"; FAIL=$((FAIL + 1))
fi

# --- Case 7: allowlisted repo proceeds past the policy gate (reaches pr list) ---
: > "$SCRATCH_DIR/gh.log"
OUT7="$(run_guard "jleechanorg/dark-factory" "$ALLOW_ONLY_DF" || true)"
if grep -q "pr list" "$SCRATCH_DIR/gh.log"; then
  echo "PASS: allowlisted repo proceeds past policy gate to pr list"; PASS=$((PASS + 1))
else
  echo "FAIL: allowlisted repo did not reach pr list (log: $(cat "$SCRATCH_DIR/gh.log"))"; FAIL=$((FAIL + 1))
fi

echo ""
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
