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
# This test mirrors the CURRENT dispatch loop in daemon/factory-af-tick.sh
# (see tests/scripts/test_factory_af_tick.sh for the established
# wrapper-mirror convention used because $R in factory-af-tick.sh is not
# env-overridable) with a stubbed bash "$R" (factory-ao-remediate.sh), then
# inserts a bead with branch=NULL and a real, config-mapped target_repo and
# asserts dispatch is attempted (no false "no repo mapping" skip).
#
# MAINTENANCE NOTE: the `IFS=$'\t' read` line and the `sqlite3 -separator`
# line below MUST be kept byte-for-byte in sync with the equivalent lines in
# daemon/factory-af-tick.sh — that pairing is the exact surface under test.
#
# Run with: bash tests/scripts/test_factory_af_tick_repo_mapping.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OVERLAY="$ROOT/daemon/factory-overlay.sh"

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

# ---------------------------------------------------------------------------
# Wrapper mirroring the CURRENT daemon/factory-af-tick.sh dispatch loop
# (repo-resolution + AO-project-resolution + remediate-spawn segment), with
# $DB -> $AFD_DB, $O -> $OVERLAY, $R -> stubbed fake-R, and the WHERE clause's
# bead_filter/pr_sql_filter/order_clause simplified to a WRAPPER_BEAD
# allowlist (same simplification tests 5-7 in test_factory_af_tick.sh use).
# The post-dispatch dispatch-record/rc-handling tail is intentionally
# omitted here — this test is scoped to the repo-mapping field-shift bug,
# not the full state-machine transition (which requires a non-empty branch
# and is exercised elsewhere in test_factory_af_tick.sh).
# ---------------------------------------------------------------------------
WRAPPER="$SCRATCH_DIR/wrapper.sh"
cat > "$WRAPPER" <<WRAP_EOF
#!/usr/bin/env bash
set -euo pipefail
O="$OVERLAY"
R="$FAKE_R"
WRAPPER_BEAD="\${AFD_WRAPPER_BEAD:-test-repo-mapping}"
dispatched=0
MAX_DISPATCH=2
while IFS='|' read -r bead_id pr branch bead_repo; do
  [ -n "\$bead_id" ] || continue
  [ "\$dispatched" -ge "\$MAX_DISPATCH" ] && break

  repo="\${bead_repo:-\${TARGET_REPO:-}}"
  if [ -z "\$repo" ]; then
    echo "[af] skip \$bead_id: no repo mapping (fail-closed, no bead_repo or TARGET_REPO)" >&2
    continue
  fi

  proj="\$(python3 - "\$CONFIG" "\$repo" <<'PY'
import sys, toml
config_path = sys.argv[1]
target_repo = sys.argv[2]
try:
    cfg = toml.load(config_path)
except Exception:
    cfg = {}
repos = cfg.get("repos", {})
if target_repo in repos:
    print(repos[target_repo].get("ao_project", ""))
    sys.exit(0)
global_target = cfg.get("target_repo")
if target_repo == global_target:
    ao_project = cfg.get("ao_project")
    if ao_project:
        print(ao_project)
        sys.exit(0)
    project = target_repo.split('/')[-1]
    if project == "worldarchitect.ai":
        project = "worldarchitect"
    print(project)
    sys.exit(0)
print("")
PY
)"

  if [ -z "\$proj" ]; then
    echo "[af] fail closed: target repo '\$repo' has no matching configured AO project. Parking bead \$bead_id." >&2
    "\$O" park "\$bead_id" "unmapped_target_repo" >/dev/null || true
    continue
  fi

  echo "[af] remediate \$bead_id PR #\$pr on \$repo in project \$proj"
  if bash "\$R" "\$bead_id" "\$pr" "\$repo" "\$proj" 2>&1; then
    dispatched=\$((dispatched + 1))
  else
    echo "[af] skip \$bead_id (ao spawn failed)" >&2
  fi
done < <(sqlite3 "\$AFD_DB" -separator '|' \
  "SELECT bead_id, pr_number, coalesce(branch,''), coalesce(target_repo,'') FROM bead_overlay
   WHERE state IN ('QUEUED','ATTESTED') AND pr_number IS NOT NULL
   AND bead_id IN ('\${WRAPPER_BEAD}')
   ORDER BY updated_at LIMIT 10;")
echo "af_dispatched=\$dispatched"
WRAP_EOF
chmod +x "$WRAPPER"

# ---------------------------------------------------------------------------
# Test: NULL branch + real, config-mapped target_repo must NOT be
# misread as "no repo mapping" (rev-wzrh regression).
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
out="$(AFD_WRAPPER_BEAD=test-repo-mapping bash "$WRAPPER" 2>&1)"

case "$out" in
  *'no repo mapping'*)
    echo "FAIL: rev-wzrh regression — false 'no repo mapping' skip for bead with a real target_repo"
    echo "--- wrapper output ---"
    echo "$out"
    echo "----------------------"
    FAIL=$((FAIL + 1))
    ;;
  *)
    echo "PASS: rev-wzrh — NULL-branch bead with real target_repo not falsely skipped as unmapped"
    PASS=$((PASS + 1))
    ;;
esac

fake_calls="$(grep -c 'fake-R.*called' "$FAKE_R_LOG" || true)"
assert "rev-wzrh: dispatch attempted (fake-R called) for NULL-branch bead" "1" "$fake_calls"
assert_grep "rev-wzrh: fake-R received the correct (unshifted) target_repo" 'repo=jleechanorg/worldarchitect\.ai proj=worldarchitect' "$FAKE_R_LOG"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
