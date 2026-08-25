#!/usr/bin/env bash
# test_factory_af_tick_repo_mapping.sh — regression test for bead rev-wzrh.
#
# Bug: daemon/factory-af-tick.sh's dispatch loop reads the per-bead SELECT
# row with `while IFS=$'\t' read -r bead_id pr branch bead_repo; do` fed by
# `sqlite3 ... -separator $'\t' "SELECT bead_id, pr_number,
# coalesce(branch,''), coalesce(target_repo,'') FROM bead_overlay ..."`.
#
# bash `read` treats a TAB in IFS as "IFS whitespace" (same class as space
# and newline) regardless of what else is in IFS, so RUNS of tabs are
# collapsed and an EMPTY field between two tabs vanishes. When `branch` is
# NULL (coalesced to ''), the emitted row is
# `bead_id<TAB>pr<TAB><TAB>bead_repo` — the collapse eats the empty branch
# field, so bead_repo's value shifts left into the `branch` variable and the
# `bead_repo` variable is left empty. The fail-closed check
# `repo="${bead_repo:-${TARGET_REPO:-}}"` then wrongly treats a bead that
# DOES have a real target_repo as unmapped, logging
# "no repo mapping (fail-closed...)" and skipping dispatch — a false skip /
# dispatch deadlock for any QUEUED bead whose branch hasn't been assigned
# yet.
#
# Fix: switch the delimiter from tab to pipe (`|`) in both the `IFS=` read
# and the `sqlite3 -separator` call — pipe is NOT "IFS whitespace" so bash
# does not collapse runs of it, preserving exact field count. bead_id
# (`^[A-Za-z0-9._-]+$`), pr_number (numeric), branch
# (`^[A-Za-z0-9._/-]+$`) and target_repo (`owner/repo`) can never contain
# `|` per this repo's validators, so pipe is a safe delimiter.
#
# HONESTY FIX (codex-skeptic follow-up on PRs #619/#620/#621): this test
# used to generate a scratch WRAPPER script that hand-mirrored the dispatch
# loop's IFS/sqlite3-separator logic instead of calling the real
# daemon/factory-af-tick.sh. That meant a revert of the pipe-delimiter fix
# in the PRODUCTION file would NOT be caught here — this test would keep
# passing against its own frozen copy of the (correct) logic forever. The
# wrapper existed because `$R` (factory-ao-remediate.sh) in the real script
# was not env-overridable, so there was no way to stub out the AO-remediate
# spawn call. Fixed by adding AFD_REMEDIATE_BIN / AFD_INTAKE_BIN env seams
# to daemon/factory-af-tick.sh (production-behavior-identical when unset —
# both still default to the real scripts) and invoking the REAL script
# directly here, with AFD_SKIP_DRIFT_CHECK=1 (the script's own documented
# test-only bypass for its Gate-0 checkout-drift guard — never set on the
# production plist) and AFD_DAEMON_BIN pointed at a stub (the existing,
# already-supported override daemon/factory-overlay.sh uses for its
# `recover-held` subcommand — this repo's CI does not build the Rust
# daemon binary, so a real invocation needs this stubbed to stay portable).
#
# This test inserts a bead with branch=NULL and a real, config-mapped
# target_repo. Issue #743 now correctly blocks it before AO spawn because an
# exact branch is required for worktree ownership preflight; it must still
# reach that preflight (not be falsely skipped as an unmapped repository).
#
# Run with: bash tests/scripts/test_factory_af_tick_repo_mapping.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"
TICK="$ROOT/daemon/factory-af-tick.sh"

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
  if grep -qE "$pattern" "$file" 2>/dev/null; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (pattern '$pattern' not found in $file)"
    FAIL=$((FAIL + 1))
  fi
}

SCRATCH_DIR="$(mktemp -d -t test-af-tick-repo.XXXXXX)"
FAKE_R_LOG="$SCRATCH_DIR/fake-r.log"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

# Stub for factory-ao-remediate.sh: records the call (bead_id/pr/repo/proj), exits 0.
FAKE_R="$SCRATCH_DIR/fake-r.sh"
cat > "$FAKE_R" <<STUB_R_EOF
#!/usr/bin/env bash
echo "[fake-R] called: bead_id=\$1 pr=\$2 repo=\${3:-} proj=\${4:-}" >> "$FAKE_R_LOG"
exit 0
STUB_R_EOF
chmod +x "$FAKE_R"
: > "$FAKE_R_LOG"

# Stub for factory-intake-from-gh.sh: the real script calls `gh issue list`
# against a live repo, which would make this test network-dependent and
# side-effecting (real `br` bead mutations from live GitHub issues). This
# test is scoped to the repo-mapping field-shift bug, not intake — no-op it.
FAKE_I="$SCRATCH_DIR/fake-i.sh"
cat > "$FAKE_I" <<'STUB_I_EOF'
#!/usr/bin/env bash
exit 0
STUB_I_EOF
chmod +x "$FAKE_I"

# Stub for the compiled Rust daemon binary that `factory-overlay.sh
# recover-held` shells out to (AFD_DAEMON_BIN, an existing override — see
# daemon/factory-overlay.sh's DAEMON_BIN resolution). This repo's CI does
# not build daemon/target/release/daemon, so a real end-to-end invocation
# of factory-af-tick.sh needs this stubbed to stay portable / fast.
FAKE_DAEMON="$SCRATCH_DIR/fake-daemon.sh"
cat > "$FAKE_DAEMON" <<'STUB_DAEMON_EOF'
#!/usr/bin/env bash
case "${1:-}" in
  recover-held) echo "recovered=0"; exit 0 ;;
  *) exit 0 ;;
esac
STUB_DAEMON_EOF
chmod +x "$FAKE_DAEMON"

fresh_db() {
  local tag="${1:-main}"
  export AFD_DB="$SCRATCH_DIR/cxdb-$tag.sqlite"
  export AFD_LOG="$SCRATCH_DIR/cxdb-$tag.jsonl"
  "$OVERLAY" init >/dev/null
}

write_config() {
  local cfg="$SCRATCH_DIR/daemon.toml"
  cat > "$cfg" <<TOML_EOF
target_repo = "jleechanorg/worldarchitect.ai"
ao_project = "worldarchitect"
max_workers = 30
max_batch = 15
TOML_EOF
  printf '%s' "$cfg"
}

run_tick() { # <tick_script_path> -> real invocation of factory-af-tick.sh (or a reverted scratch copy)
  local tick_bin="$1"
  AFD_REMEDIATE_BIN="$FAKE_R" \
  AFD_INTAKE_BIN="$FAKE_I" \
  AFD_DAEMON_BIN="$FAKE_DAEMON" \
  AFD_BEAD_FILTER=test-repo-mapping \
  AFD_SKIP_DRIFT_CHECK=1 \
  bash "$tick_bin" 2>&1
}

# ---------------------------------------------------------------------------
# Test: NULL branch + real, config-mapped target_repo must NOT be
# misread as "no repo mapping" (rev-wzrh regression) — exercised against the
# REAL daemon/factory-af-tick.sh, not a hand-mirrored copy.
# ---------------------------------------------------------------------------
fresh_db reposkip
export CONFIG="$(write_config)"
"$OVERLAY" intake-upsert test-repo-mapping 'rev-wzrh: NULL branch + real target_repo' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=4242, target_repo='jleechanorg/worldarchitect.ai' WHERE bead_id='test-repo-mapping';"
"$OVERLAY" route-record test-repo-mapping STANDARD_PATH 'drive-existing-pr' >/dev/null

# Sanity: confirm the row really has branch=NULL / target_repo set, so the
# test is actually exercising the coalesce('') + tab-collapse scenario.
row="$(sqlite3 "$AFD_DB" "SELECT branch IS NULL, target_repo FROM bead_overlay WHERE bead_id='test-repo-mapping';")"
assert "fixture: branch IS NULL and target_repo set" "1|jleechanorg/worldarchitect.ai" "$row"

: > "$FAKE_R_LOG"
out="$(run_tick "$TICK")"

case "$out" in
  *'no repo mapping'*)
    echo "FAIL: rev-wzrh regression — false 'no repo mapping' skip for bead with a real target_repo"
    echo "--- tick output ---"
    echo "$out"
    echo "--------------------"
    FAIL=$((FAIL + 1))
    ;;
  *)
    echo "PASS: rev-wzrh — NULL-branch bead with real target_repo not falsely skipped as unmapped"
    PASS=$((PASS + 1))
    ;;
esac

fake_calls="$(grep -c 'fake-R.*called' "$FAKE_R_LOG" || true)"
assert "issue #743: NULL branch does not invoke AO remediation" "0" "$fake_calls"
assert_grep "issue #743: NULL branch gets structured missing_branch telemetry" '"eventType": "TASK_DISPATCH_BLOCKED".*"reason": "missing_branch"' "$AFD_LOG"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
