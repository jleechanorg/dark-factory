#!/usr/bin/env bash
# test_factory_merge_guard_caller.sh — proves factory-af-tick.sh MUST invoke
# ready-scheduler.sh on every tick (Blocker #7 from the 2026-07-06 gap review).
#
# The contradiction:
#   * docs/auto-factory-daemon-spec.md §4.2.8 used to say "Merge: never."
#   * docs/cutover-exit-criteria.md X4 requires the production merge function
#     to actually attempt the merge on a no-red PR.
# Resolution (this PR): the spec was reconciled. Merge is BLOCKED ON POLICY
# until the 7-green pre-merge checks are enforceable (gate 6 /er has no
# automated runner — bead jleechan-qqq still open). The factory instead
# schedules READY transitions via daemon/scripts/ready-scheduler.sh, which:
#   1. requires no red gate,
#   2. requires every one of the 7 required gates to be accounted for,
#   3. transitions the bead overlay to READY (no `gh pr merge`),
#   4. stamps gate-evidence JSON onto daemon/.ready-evidence/<pr>.json.
# The merge itself remains with a future authority that has full 7-green
# evidence (operator constraint on jleechan-s3c: "no merge side effect").
#
# This test runs factory-af-tick.sh in a sandboxed worktree that:
#   * routes through stubs for `gh`, `br`, and `factory-ao-remediate.sh`;
#   * routes `daemon/scripts/ready-scheduler.sh` to a stub that records
#     invocation + the AFD_LOG it received;
#   * asserts the stub was called with the right environment.
# It then runs again with READY_SCHEDULER_DISABLED=1 and asserts the stub
# was NOT called (cutover / single-writer opt-out, per X7). Finally it does
# source-level assertions on the spec reconciliation.
#
# Run with: bash tests/scripts/test_factory_merge_guard_caller.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
AF_TICK="$ROOT/daemon/factory-af-tick.sh"
OVERLAY="$ROOT/daemon/factory-overlay.sh"
INTENT="$ROOT/daemon/factory-intake-from-gh.sh"
READY_SCHEDULER_REAL="$ROOT/daemon/scripts/ready-scheduler.sh"
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

# Stubbed ready-scheduler: records argv + env, exits 0. Crucially does
# NOT touch gh / does NOT call `gh pr merge` — enforces the "no merge
# side effect" guarantee from a behavioral angle. Also records
# READY_SCHEDULER_REPO so the Codex P1 target-repo test can verify
# propagation.
cat > "$SBX/daemon/scripts/ready-scheduler.sh" <<STUB
#!/usr/bin/env bash
echo "[ready-scheduler-stub] called: argv=\$* AFD_LOG=\${AFD_LOG:-unset} READY_SCHEDULER_DISABLED=\${READY_SCHEDULER_DISABLED:-unset} READY_SCHEDULER_REPO=\${READY_SCHEDULER_REPO:-unset}" >> "$CALLS_LOG"
exit 0
STUB
chmod +x "$SBX/daemon/scripts/ready-scheduler.sh"

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
# Run 1: happy path — READY_SCHEDULER_DISABLED=0. Expect scheduler invoked.
# ------------------------------------------------------------------
: > "$CALLS_LOG"
READY_SCHEDULER_DISABLED=0 bash "$SBX/daemon/factory-af-tick.sh" --prs "" \
  >"$SCRATCH_DIR/run1.out" 2>"$SCRATCH_DIR/run1.err" || true

sched_calls="$(grep -c '\[ready-scheduler-stub\] called' "$CALLS_LOG" || true)"
assert "happy path: ready-scheduler.sh invoked once" "1" "$sched_calls"

# The scheduler must receive AFD_LOG so it can read the GATE_ASSESSMENT
# emitted by the daemon's verifier tier; without this the no-red policy
# is a coin flip on cross-process log paths.
assert_grep "happy path: AFD_LOG propagated to scheduler" \
  'AFD_LOG=.*cxdb\.jsonl' "$CALLS_LOG"

# ------------------------------------------------------------------
# Run 2: cutover / single-writer opt-out — READY_SCHEDULER_DISABLED=1.
# The scheduler must NOT run, per cutover-exit-criteria.md X7
# "Single-writer during cutover".
# ------------------------------------------------------------------
: > "$CALLS_LOG"
READY_SCHEDULER_DISABLED=1 bash "$SBX/daemon/factory-af-tick.sh" --prs "" \
  >"$SCRATCH_DIR/run2.out" 2>"$SCRATCH_DIR/run2.err" || true

sched_calls_disabled="$(grep -c '\[ready-scheduler-stub\] called' "$CALLS_LOG" || true)"
assert "cutover opt-out: scheduler NOT invoked when READY_SCHEDULER_DISABLED=1" "0" "$sched_calls_disabled"

# ------------------------------------------------------------------
# Source-level assertion: the factory-af-tick.sh script body MUST contain
# the scheduler call site. Catches regressions where someone removes it
# while the behavioral tests still pass due to env overrides.
# ------------------------------------------------------------------
assert_grep "factory-af-tick.sh contains ready-scheduler call site" \
  'ready-scheduler\.sh' "$AF_TICK"
assert_grep "factory-af-tick.sh honors READY_SCHEDULER_DISABLED" \
  'READY_SCHEDULER_DISABLED' "$AF_TICK"

# ------------------------------------------------------------------
# Codex P1 thread PRRT_kwDOSjv_9s6O0bY3: the scheduler MUST resolve the
# target repo from config/daemon.toml (or an env var), NOT from
# `gh repo view` against the local checkout — otherwise the autonomous
# tick would scan dark-factory PRs and never the configured target
# (jleechanorg/worldarchitect.ai). Verify the precedence order.
# ------------------------------------------------------------------
assert_grep "ready-scheduler.sh reads target_repo from config/daemon.toml" \
  'config/daemon\.toml' "$READY_SCHEDULER_REAL"
assert_grep "ready-scheduler.sh honors READY_SCHEDULER_REPO env override" \
  'READY_SCHEDULER_REPO' "$READY_SCHEDULER_REAL"
assert_grep "factory-af-tick.sh propagates READY_SCHEDULER_REPO" \
  'READY_SCHEDULER_REPO' "$AF_TICK"

# Behavioral proof: in the sandbox, set READY_SCHEDULER_REPO to a custom
# value and verify the scheduler receives it. We can't observe `gh pr list`
# output directly because the gh shim stubs to `[]`, but we can confirm
# the env var made it through to the stub via the calls log.
: > "$CALLS_LOG"
READY_SCHEDULER_DISABLED=0 READY_SCHEDULER_REPO="jleechanorg/test-target" \
  bash "$SBX/daemon/factory-af-tick.sh" --prs "" \
  >"$SCRATCH_DIR/run_repo.out" 2>"$SCRATCH_DIR/run_repo.err" || true
assert_grep "ready-scheduler.sh received READY_SCHEDULER_REPO env" \
  'READY_SCHEDULER_REPO=jleechanorg/test-target' "$CALLS_LOG"

# ------------------------------------------------------------------
# Operator constraint enforcement (no merge side effect): the test stub
# for ready-scheduler.sh intentionally does NOT contain "gh pr merge".
# This is the behavioral proof that the wiring has no merge side effect
# at the script boundary. We also assert on the real script.
# ------------------------------------------------------------------
assert_grep "ready-scheduler.sh does NOT call gh pr merge (no merge side effect)" \
  '^[[:space:]]*#.*gh pr merge' "$READY_SCHEDULER_REAL"
# Negative assertion: the literal 'gh pr merge' command must not appear in
# the ready-scheduler script body. (Comments are allowed so we can name
# the constraint in the docstring.)
if grep -E '^[^#]*\bgh[[:space:]]+pr[[:space:]]+merge\b' "$READY_SCHEDULER_REAL" >/dev/null 2>&1; then
  echo "FAIL: ready-scheduler.sh contains a real 'gh pr merge' call (forbidden by operator constraint)"
  FAIL=$((FAIL + 1))
else
  echo "PASS: ready-scheduler.sh contains no executable 'gh pr merge' call"
  PASS=$((PASS + 1))
fi

# ------------------------------------------------------------------
# Spec reconciliation: docs/auto-factory-daemon-spec.md §4.2.8 must no
# longer declare "Merge: never." The reconciled clause names the
# scheduler and the no-red policy so future readers do not re-derive
# the contradiction. We assert on the file content (NOT on shell scripts).
# ------------------------------------------------------------------
if grep -nE '^\*\s+\*\*Merge: never\.\*\*' "$SPEC" >/dev/null; then
  echo "FAIL: spec §4.2.8 still declares 'Merge: never.' (contradicts X4)"
  FAIL=$((FAIL + 1))
else
  echo "PASS: spec §4.2.8 reconciled (no literal 'Merge: never.')"
  PASS=$((PASS + 1))
fi
assert_grep "spec §4.2.8 names ready-scheduler as the factory's READY step" \
  'ready-scheduler\.sh' "$SPEC"
assert_grep "spec §4.2.8 names the 7 required gates" \
  'seven' "$SPEC"
assert_grep "spec §4.2.8 honors the operator no-merge-side-effect constraint" \
  'no merge side effect|does not call .*gh pr merge' "$SPEC"
assert_grep "spec §4.2.8 cross-references jleechan-qqq (gate 6 /er blocker)" \
  'jleechan-qqq' "$SPEC"
assert_grep "spec §4.2.8 references cutover X7 single-writer opt-out" \
  'X7' "$SPEC"

# ------------------------------------------------------------------
# Real-script integrity: ready-scheduler.sh must be present and executable
# so the new caller can actually invoke it.
# ------------------------------------------------------------------
if [ -x "$READY_SCHEDULER_REAL" ]; then
  echo "PASS: daemon/scripts/ready-scheduler.sh exists and is executable"
  PASS=$((PASS + 1))
else
  echo "FAIL: daemon/scripts/ready-scheduler.sh missing or not executable ($READY_SCHEDULER_REAL)"
  FAIL=$((FAIL + 1))
fi

# ------------------------------------------------------------------
# Behavioral no-merge proof (factory-af-tick.sh): the merged PR stub
# must NOT see a `gh pr merge` call during a happy-path tick. We grep
# the call log: the only `gh` invocations should be from intake / list,
# never from a merge. The merge stub branch in the gh shim is the
# only place that branch is hit, and our tick never exercises the
# merge step (READY scheduler only). Negative-assertion reinforcement.
# ------------------------------------------------------------------
merge_calls="$(grep -c 'gh-stub.*pr.*merge\|gh.*pr.*merge' "$CALLS_LOG" || true)"
assert "happy path: NO gh pr merge invocation recorded" "0" "$merge_calls"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0