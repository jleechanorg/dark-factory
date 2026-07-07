#!/usr/bin/env bash
# test_factory_merge_guard_caller.sh — proves factory-af-tick.sh MUST invoke
# auto-merge-guard.sh on every tick (Blocker #7 from the 2026-07-06 gap review).
#
# The contradiction:
#   * docs/auto-factory-daemon-spec.md §4.2.8 used to say "Merge: never."
#   * docs/cutover-exit-criteria.md X4 requires the production merge function
#     to actually attempt the merge on a no-red PR.
# Resolution (this PR): the spec was reconciled to "Merge: gated via
# auto-merge-guard.sh (no-red policy)". The merge-guard now needs an automatic
# caller — otherwise 4 of 4 factory PRs the system has produced were merged by
# jleechan2015 by hand and the zero-touch goal stays structurally unreachable.
#
# This test runs factory-af-tick.sh in a sandboxed worktree that:
#   * routes through stubs for `gh`, `br`, and `factory-ao-remediate.sh`;
#   * routes `daemon/scripts/auto-merge-guard.sh` to a stub that records
#     invocation + the AFD_LOG it received;
#   * asserts the stub was called with the right environment.
# It then runs again with AUTO_MERGE_DISABLED=1 and asserts the stub was NOT
# called (cutover / single-writer opt-out, per X7). Finally it does source-
# level assertions on the spec reconciliation.
#
# Run with: bash tests/scripts/test_factory_merge_guard_caller.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
AF_TICK="$ROOT/daemon/factory-af-tick.sh"
OVERLAY="$ROOT/daemon/factory-overlay.sh"
INTENT="$ROOT/daemon/factory-intake-from-gh.sh"
MERGE_GUARD_REAL="$ROOT/daemon/scripts/auto-merge-guard.sh"
SPEC="$ROOT/docs/auto-factory-daemon-spec.md"

PASS=0; FAIL=0
assert() {
  local name="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (expected '$expected', got '$actual')"
    FAIL=$((FAIL + 1))
  fi
}
assert_grep() {
  local name="$1" pattern="$2" file="$3"
  if [ -f "$file" ] && grep -qE "$pattern" "$file" 2>/dev/null; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (pattern '$pattern' not found in $file)"
    FAIL=$((FAIL + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-merge-guard.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

# ------------------------------------------------------------------
# Sandbox: a self-contained tree whose factory-af-tick.sh runs without
# touching the real repo's gh/br state. The script uses `cd "$ROOT"` and
# resolves paths relative to its own location, so we symlink all repo
# scripts into the sandbox tree verbatim, except for the three we want to
# stub.
# ------------------------------------------------------------------
SBX="$SCRATCH_DIR/repo"
mkdir -p "$SBX/daemon" "$SBX/daemon/scripts" "$SBX/.beads"
ln -sf "$OVERLAY"        "$SBX/daemon/factory-overlay.sh"
ln -sf "$AF_TICK"        "$SBX/daemon/factory-af-tick.sh"
ln -sf "$INTENT"         "$SBX/daemon/factory-intake-from-gh.sh"
ln -sf "$ROOT/daemon/factory-ao-bin.sh" "$SBX/daemon/factory-ao-bin.sh"
ln -sf "$ROOT/daemon/contracts" "$SBX/daemon/contracts"

# Stubbed scripts we control. Each records its invocation in CALLS_LOG.
CALLS_LOG="$SCRATCH_DIR/calls.log"
: > "$CALLS_LOG"

cat > "$SBX/daemon/factory-ao-remediate.sh" <<STUB
#!/usr/bin/env bash
echo "[remediate-stub] called: bead=\$1 pr=\$2" >> "$CALLS_LOG"
exit 0
STUB
chmod +x "$SBX/daemon/factory-ao-remediate.sh"

# Stubbed auto-merge-guard.sh: records argv + env, exits 0.
cat > "$SBX/daemon/scripts/auto-merge-guard.sh" <<STUB
#!/usr/bin/env bash
echo "[merge-guard-stub] called: argv=\$* AFD_LOG=\${AFD_LOG:-unset} AUTO_MERGE_DISABLED=\${AUTO_MERGE_DISABLED:-unset}" >> "$CALLS_LOG"
exit 0
STUB
chmod +x "$SBX/daemon/scripts/auto-merge-guard.sh"

# Stub PATH: gh / br / ao / sqlite3 stubs that record calls and never hit
# the network. (sqlite3 is needed for the overlay's sqlite3 calls.)
STUBDIR="$SCRATCH_DIR/stubs"
mkdir -p "$STUBDIR"
ln -sf "$(command -v sqlite3)" "$STUBDIR/sqlite3"

cat > "$STUBDIR/gh" <<'STUB'
#!/usr/bin/env bash
# Minimal gh shim: every command returns an empty repo / empty list / etc.
# This keeps the intake + dispatch loop harmless for the test.
case "${1:-}${2:-}${3:-}" in
  *repo*view*) echo '{"nameWithOwner":"jleechanorg/dark-factory"}' ;;
  *issue*list*) echo '[]' ;;
  *pr*list*)    echo '[]' ;;
  *pr*view*)    echo '{"state":"OPEN","mergeable":"MERGEABLE","headRefName":""}' ;;
  *pr*checks*)  echo '' ;;
  *pr*merge*)   echo 'merged' ;;
  *)            echo '[]' ;;
esac
STUB
chmod +x "$STUBDIR/gh"

cat > "$STUBDIR/br" <<STUB
#!/usr/bin/env bash
echo "[br-stub] \$*" >> "$CALLS_LOG"
exit 0
STUB
chmod +x "$STUBDIR/br"

cat > "$STUBDIR/ao" <<STUB
#!/usr/bin/env bash
echo "[ao-stub] \$*" >> "$CALLS_LOG"
echo '[]'
exit 0
STUB
chmod +x "$STUBDIR/ao"

# Beads DB + log live in the sandbox so we never touch the real ones.
export BR_DB="$SBX/.beads/beads.db"
export AFD_DB="$SCRATCH_DIR/cxdb.sqlite"
export AFD_LOG="$SCRATCH_DIR/cxdb.jsonl"
export PATH="$STUBDIR:$PATH"

# Config: max_workers=30, max_batch=15 (the live defaults).
SCRATCH_CFG="$SCRATCH_DIR/daemon-cap.toml"
cat > "$SCRATCH_CFG" <<TOML_EOF
max_workers = 30
max_batch = 15
TOML_EOF
export CONFIG="$SCRATCH_CFG"

# Init overlay (creates the sqlite tables the tick reads).
"$OVERLAY" init >/dev/null

# ------------------------------------------------------------------
# Run 1: happy path — AUTO_MERGE_DISABLED=0. Expect merge-guard invoked.
# ------------------------------------------------------------------
: > "$CALLS_LOG"
AUTO_MERGE_DISABLED=0 bash "$SBX/daemon/factory-af-tick.sh" --prs "" \
  >"$SCRATCH_DIR/run1.out" 2>"$SCRATCH_DIR/run1.err" || true

guard_calls="$(grep -c '\[merge-guard-stub\] called' "$CALLS_LOG" || true)"
assert "happy path: auto-merge-guard.sh invoked once" "1" "$guard_calls"

# The merge-guard must receive AFD_LOG so it can read the GATE_ASSESSMENT
# emitted by the daemon's verifier tier; without this the no-red policy is
# a coin flip on cross-process log paths.
assert_grep "happy path: AFD_LOG propagated to merge-guard" \
  'AFD_LOG=.*cxdb\.jsonl' "$CALLS_LOG"

# ------------------------------------------------------------------
# Run 2: cutover / single-writer opt-out — AUTO_MERGE_DISABLED=1.
# The merge-guard must NOT run, per cutover-exit-criteria.md X7
# "Single-writer during cutover".
# ------------------------------------------------------------------
: > "$CALLS_LOG"
AUTO_MERGE_DISABLED=1 bash "$SBX/daemon/factory-af-tick.sh" --prs "" \
  >"$SCRATCH_DIR/run2.out" 2>"$SCRATCH_DIR/run2.err" || true

guard_calls_disabled="$(grep -c '\[merge-guard-stub\] called' "$CALLS_LOG" || true)"
assert "cutover opt-out: merge-guard NOT invoked when AUTO_MERGE_DISABLED=1" "0" "$guard_calls_disabled"

# ------------------------------------------------------------------
# Source-level assertion: the factory-af-tick.sh script body MUST contain
# the merge-guard call site. Catches regressions where someone removes it
# while the behavioral tests still pass due to env overrides.
# ------------------------------------------------------------------
assert_grep "factory-af-tick.sh contains merge-guard call site" \
  'auto-merge-guard\.sh' "$AF_TICK"
assert_grep "factory-af-tick.sh honors AUTO_MERGE_DISABLED" \
  'AUTO_MERGE_DISABLED' "$AF_TICK"

# ------------------------------------------------------------------
# Spec reconciliation: docs/auto-factory-daemon-spec.md §4.2.8 must no
# longer declare "Merge: never." The reconciled clause names the guard
# and the no-red policy so future readers do not re-derive the
# contradiction. We assert on the file content (NOT on shell scripts).
# ------------------------------------------------------------------
if grep -nE '^\*\s+\*\*Merge: never\.\*\*' "$SPEC" >/dev/null; then
  echo "FAIL: spec §4.2.8 still declares 'Merge: never.' (contradicts X4)"
  FAIL=$((FAIL + 1))
else
  echo "PASS: spec §4.2.8 reconciled (no literal 'Merge: never.')"
  PASS=$((PASS + 1))
fi
assert_grep "spec §4.2.8 names auto-merge-guard as merge authority" \
  'auto-merge-guard' "$SPEC"
assert_grep "spec §4.2.8 names no-red policy" \
  'no-red' "$SPEC"

# ------------------------------------------------------------------
# Merge-guard script integrity: the real auto-merge-guard.sh must be
# present and executable so the new caller can actually invoke it.
# ------------------------------------------------------------------
if [ -x "$MERGE_GUARD_REAL" ]; then
  echo "PASS: daemon/scripts/auto-merge-guard.sh exists and is executable"
  PASS=$((PASS + 1))
else
  echo "FAIL: daemon/scripts/auto-merge-guard.sh missing or not executable ($MERGE_GUARD_REAL)"
  FAIL=$((FAIL + 1))
fi

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0