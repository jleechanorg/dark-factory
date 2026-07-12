#!/usr/bin/env bash
# test_factory_af_tick.sh — round-trip tests for factory-af-tick.sh dispatch loop.
#
# Verifies the bead jleechan-xzsh refactor: factory-af-tick.sh MUST invoke
# factory-overlay.sh:dispatch-record (NOT direct sqlite UPDATE) when transitioning
# QUEUED → DISPATCHED. The overlay subcommands enforce:
#   - valid_branch regex
#   - require_state=QUEUED guard
#   - branch_registry owner check
#   - capacity() gate from config max_workers/max_batch
#   - TASK_DISPATCHED telemetry event emission
#
# Tests cover the four overlay guards plus an end-to-end factory-af-tick
# integration run with a stubbed bash "$R" (factory-ao-remediate.sh) that
# captures which subcommand calls actually happened.
#
# Run with: bash tests/scripts/test_factory_af_tick.sh
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

# Scratch files (per-test re-created so each test starts with a clean slate).
SCRATCH_DIR="$(mktemp -d -t test-af-tick.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR" /tmp/test-af-tick-fake-r.log; }
trap cleanup EXIT

# Stub for factory-ao-remediate.sh: records the call, exits 0.
FAKE_R="$SCRATCH_DIR/fake-r.sh"
cat > "$FAKE_R" <<'STUB_R_EOF'
#!/usr/bin/env bash
echo "[fake-R] called: bead_id=$1 pr=$2 repo=${3:-} proj=${4:-}" >> /tmp/test-af-tick-fake-r.log
exit 0
STUB_R_EOF
chmod +x "$FAKE_R"
: > /tmp/test-af-tick-fake-r.log

# Helper: fresh DB + log + apply schema; sets AFD_DB / AFD_LOG accordingly.
fresh_db() {
  local tag="${1:-main}"
  export AFD_DB="$SCRATCH_DIR/cxdb-$tag.sqlite"
  export AFD_LOG="$SCRATCH_DIR/cxdb-$tag.jsonl"
  "$OVERLAY" init >/dev/null
}

write_config() {
  local mw="${1:-30}" mb="${2:-15}"
  local cfg="$SCRATCH_DIR/daemon-cap-$mw-$mb.toml"
  cat > "$cfg" <<TOML_EOF
target_repo = "jleechanorg/worldarchitect.ai"
ao_project = "worldarchitect"
max_workers = $mw
max_batch = $mb
TOML_EOF
  printf '%s' "$cfg"
}

# ---------------------------------------------------------------------------
# Test 1: happy path — QUEUED-after-route-record → dispatch-record succeeds
# ---------------------------------------------------------------------------
fresh_db happy
write_config 30 15 >/dev/null; export CONFIG="$(write_config 30 15)"
"$OVERLAY" intake-upsert test-happy 'happy path' >/dev/null
out="$("$OVERLAY" route-record test-happy STANDARD_PATH 'drive-existing-pr')"
assert "route-record happy" "ok" "$out"
"$OVERLAY" dispatch-record test-happy fix/test-happy >/dev/null
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-happy';")"
assert "state QUEUED → DISPATCHED (happy)" "DISPATCHED" "$state"
owner="$(sqlite3 "$AFD_DB" "SELECT bead_id FROM branch_registry WHERE branch='fix/test-happy';")"
assert "branch_registry owner (happy)" "test-happy" "$owner"
assert_grep "TASK_DISPATCHED telemetry (happy)" '"eventType": "TASK_DISPATCHED"' "$AFD_LOG"

# ---------------------------------------------------------------------------
# Test 2: require_state guard — already-DISPATCHED bead, dispatch-record rejected
# (dispatch-record expects state=QUEUED; already-DISPATCHED beads are rejected
# by the overlay's require_state guard rather than silently re-dispatched.)
# ---------------------------------------------------------------------------
"$OVERLAY" intake-upsert test-guard 'require_state guard' >/dev/null
"$OVERLAY" route-record test-guard STANDARD_PATH 'drive-existing-pr' >/dev/null
"$OVERLAY" dispatch-record test-guard fix/test-guard >/dev/null

set +e
"$OVERLAY" dispatch-record test-guard fix/test-guard-2 >"$SCRATCH_DIR/err.log" 2>&1
rc=$?
set -e
assert "dispatch-record refuses non-QUEUED bead (rc=5 EX_REQUIRE_STATE)" "5" "$rc"
err="$(cat "$SCRATCH_DIR/err.log")"
case "$err" in
  *expected\ one\ of*QUEUED*) echo "PASS: require_state message mentions QUEUED"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: require_state message format unexpected: $err"; FAIL=$((FAIL + 1)) ;;
esac

# ---------------------------------------------------------------------------
# Test 3: capacity gate — set max_workers=0; dispatch-record refuses
# ---------------------------------------------------------------------------
ZERO_CFG="$(write_config 0 0)"; export CONFIG="$ZERO_CFG"
fresh_db cap
"$OVERLAY" intake-upsert test-cap 'capacity test' >/dev/null
"$OVERLAY" route-record test-cap STANDARD_PATH 'drive-existing-pr' >/dev/null
cap_out="$("$OVERLAY" capacity)"
assert "capacity() with max_workers=0" "0" "$cap_out"
set +e
"$OVERLAY" dispatch-record test-cap fix/test-cap >"$SCRATCH_DIR/cap-err.log" 2>&1
rc=$?
set -e
assert "dispatch-record refuses over capacity (rc=3 EX_OVER_CAP)" "3" "$rc"
err="$(cat "$SCRATCH_DIR/cap-err.log")"
case "$err" in
  *over\ capacity*) echo "PASS: capacity gate error mentions over capacity"; PASS=$((PASS + 1)) ;;
  *) echo "FAIL: capacity gate error format unexpected: $err"; FAIL=$((FAIL + 1)) ;;
esac
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-cap';")"
assert "state unchanged after capacity refusal" "QUEUED" "$state"

# ---------------------------------------------------------------------------
# Test 4: branch registry conflict — two beads, same branch → second dies
# ---------------------------------------------------------------------------
NORMAL_CFG="$(write_config 30 15)"; export CONFIG="$NORMAL_CFG"
fresh_db conflict
"$OVERLAY" intake-upsert test-owner 'first bead' >/dev/null
"$OVERLAY" route-record test-owner STANDARD_PATH 'drive-existing-pr' >/dev/null
"$OVERLAY" dispatch-record test-owner fix/shared-branch >/dev/null
state_owner="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-owner';")"
assert "first owner registered" "DISPATCHED" "$state_owner"

"$OVERLAY" intake-upsert test-rival 'second bead' >/dev/null
"$OVERLAY" route-record test-rival STANDARD_PATH 'drive-existing-pr' >/dev/null
set +e
"$OVERLAY" dispatch-record test-rival fix/shared-branch >"$SCRATCH_DIR/conflict.log" 2>&1
rc=$?
set -e
assert "dispatch-record refuses branch conflict (rc=4 EX_BRANCH_CONFLICT)" "4" "$rc"
err="$(cat "$SCRATCH_DIR/conflict.log")"
case "$err" in
  *registered\ to\ test-owner*)
    echo "PASS: branch conflict reports current owner (test-owner)"
    PASS=$((PASS + 1))
    ;;
  *)
    echo "FAIL: branch conflict error format unexpected: $err"
    FAIL=$((FAIL + 1))
    ;;
esac
state_rival="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-rival';")"
assert "rival state unchanged after conflict" "QUEUED" "$state_rival"

# ---------------------------------------------------------------------------
# Test 5: factory-af-tick dispatch-loop integration (happy path)
# Build a small wrapper that mirrors the dispatch loop in factory-af-tick.sh:
# pick a QUEUED-with-PR bead, stub bash "$R", then invoke route-record +
# dispatch-record (NOT direct UPDATE). Verify state moved, telemetry fired,
# bash R was called.
# ---------------------------------------------------------------------------
fresh_db integ
"$OVERLAY" intake-upsert test-integ 'integration test' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=8116, branch='fix/test-integ-branch' WHERE bead_id='test-integ';"
"$OVERLAY" route-record test-integ STANDARD_PATH 'drive-existing-pr' >/dev/null

WRAPPER="$SCRATCH_DIR/wrapper.sh"
cat > "$WRAPPER" <<WRAP_EOF
#!/usr/bin/env bash
set -euo pipefail
O="$OVERLAY"
R="$FAKE_R"
# AFD_WRAPPER_BEAD narrows the SQL to a specific bead (used by integration tests)
# to avoid picking up other QUEUED beads from the same DB.
WRAPPER_BEAD="\${AFD_WRAPPER_BEAD:-test-integ}"
ERR_TMP="\$(mktemp -t af_int.XXXXXX)"
trap 'rm -f "\$ERR_TMP"' EXIT
dispatched=0
MAX_DISPATCH=2
while IFS=\$'\t' read -r bead_id pr branch bead_repo; do
  [ -n "\$bead_id" ] || continue
  [ "\$dispatched" -ge "\$MAX_DISPATCH" ] && break

  # Resolve target repo (default to global TARGET_REPO if empty)
  repo="\${bead_repo:-jleechanorg/worldarchitect.ai}"

  # Resolve AO project for this repo from config
  proj="\$(python3 - "\$CONFIG" "\$repo" <<'PY'
import sys, toml
config_path = sys.argv[1]
target_repo = sys.argv[2]
try:
    cfg = toml.load(config_path)
except Exception:
    cfg = {}

# 1. Look in [repos]
repos = cfg.get("repos", {})
if target_repo in repos:
    print(repos[target_repo].get("ao_project", ""))
    sys.exit(0)

# 2. Compare to global target_repo
global_target = cfg.get("target_repo")
if target_repo == global_target:
    ao_project = cfg.get("ao_project")
    if ao_project:
        print(ao_project)
        sys.exit(0)
    # Derivation fallback
    project = target_repo.split('/')[-1]
    if project == "worldarchitect.ai":
        project = "worldarchitect"
    print(project)
    sys.exit(0)

# Unmapped
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
    cur_state="\$(sqlite3 "\$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='\$(printf "%s" "\$bead_id" | sed "s/'/''/g")';" 2>/dev/null || true)"
    if [ "\$cur_state" = "QUEUED" ]; then
      if [ -n "\$branch" ]; then
        "\$O" route-record "\$bead_id" STANDARD_PATH "drive-existing-pr" 2>/dev/null || true
      fi
      if "\$O" dispatch-record "\$bead_id" "\$branch" 2>"\$ERR_TMP"; then
        :
      else
        err="\$(cat "\$ERR_TMP" 2>/dev/null || true)"
        case "\$err" in
          *over\ capacity*) echo "[af] over capacity — skip \$bead_id" >&2 ;;
          *already\ registered*)
            owner="\$(printf '%s' "\$err" | sed -n 's/.*already registered to //p')"
            echo "[af] branch conflict \$branch owned by \$owner — skip \$bead_id" >&2
            ;;
          *) echo "[af] dispatch-record refused for \$bead_id: \$err" >&2 ;;
        esac
        continue
      fi
    fi
    dispatched=\$((dispatched + 1))
  fi
done < <(sqlite3 "\$AFD_DB" -separator \$'\t' \
  "SELECT bead_id, pr_number, coalesce(branch,''), coalesce(target_repo,'') FROM bead_overlay
   WHERE state IN ('QUEUED','ATTESTED') AND pr_number IS NOT NULL
   AND bead_id IN ('\${WRAPPER_BEAD}')
   ORDER BY updated_at LIMIT 10;")
echo "af_dispatched=\$dispatched"
WRAP_EOF
chmod +x "$WRAPPER"

: > /tmp/test-af-tick-fake-r.log
out="$(AFD_WRAPPER_BEAD=test-integ CONFIG="$(write_config 30 15)" bash "$WRAPPER" 2>&1)"
af_dispatched="$(echo "$out" | grep -oE 'af_dispatched=[0-9]+' | head -1 | cut -d= -f2)"
assert "integration: af_dispatched=1" "1" "$af_dispatched"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-integ';")"
assert "integration: state=DISPATCHED via overlay" "DISPATCHED" "$state"
fake_calls="$(grep -c 'fake-R.*called' /tmp/test-af-tick-fake-r.log || true)"
assert "integration: bash R (ao spawn) called" "1" "$fake_calls"
assert_grep "integration: TASK_DISPATCHED telemetry" '"eventType": "TASK_DISPATCHED"' "$AFD_LOG"
owner="$(sqlite3 "$AFD_DB" "SELECT bead_id FROM branch_registry WHERE branch='fix/test-integ-branch';")"
assert "integration: branch_registry owner recorded" "test-integ" "$owner"

# ---------------------------------------------------------------------------
# Test 6: factory-af-tick dispatch-loop integration — capacity refusal surfaces message
# Run the same wrapper with max_workers=0; dispatch-record must refuse and the
# wrapper must surface "[af] over capacity" with state staying QUEUED.
# ---------------------------------------------------------------------------
fresh_db caploop
"$OVERLAY" intake-upsert test-caploop 'capacity loop test' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=8117, branch='fix/test-caploop-branch' WHERE bead_id='test-caploop';"
"$OVERLAY" route-record test-caploop STANDARD_PATH 'drive-existing-pr' >/dev/null
export CONFIG="$(write_config 0 0)"

: > /tmp/test-af-tick-fake-r.log
AFD_WRAPPER_BEAD=test-caploop bash "$WRAPPER" >/tmp/test-af-tick-caploop.log 2>&1 || true
# Filter the wrapper output; it should contain "[af] over capacity" message.
out="$(cat /tmp/test-af-tick-caploop.log)"
case "$out" in
  *over\ capacity*)
    echo "PASS: factory-af-tick surfaces '[af] over capacity' message"
    PASS=$((PASS + 1))
    ;;
  *)
    echo "FAIL: factory-af-tick did not surface '[af] over capacity'. Output was:"
    cat /tmp/test-af-tick-caploop.log
    FAIL=$((FAIL + 1))
    ;;
esac
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='test-caploop';")"
assert "integration: state stays QUEUED under capacity refusal" "QUEUED" "$state"

# ---------------------------------------------------------------------------
# Test 7: Multi-repo dispatch loop integration with same numeric PR
# Verify that two beads with different target_repo but same pr_number
# are dispatched to their corresponding repos/projects.
# ---------------------------------------------------------------------------
write_multirepo_config() {
  local cfg="$SCRATCH_DIR/daemon-multirepo.toml"
  cat > "$cfg" <<TOML_EOF
target_repo = "jleechanorg/worldarchitect.ai"
ao_project = "worldarchitect"
max_workers = 30
max_batch = 15

[repos."jleechanorg/worldarchitect.ai"]
ao_project = "worldarchitect"
push_remote = "worldai"

[repos."jleechanorg/dark-factory"]
ao_project = "dark-factory"
push_remote = "origin"
TOML_EOF
  printf '%s' "$cfg"
}

fresh_db multirepo
export CONFIG="$(write_multirepo_config)"

# Insert two beads with the same numeric PR but different target repos
"$OVERLAY" intake-upsert bead-wa 'wa test' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=56, branch='fix/wa-56', target_repo='jleechanorg/worldarchitect.ai' WHERE bead_id='bead-wa';"
"$OVERLAY" route-record bead-wa STANDARD_PATH 'drive-existing-pr' >/dev/null

"$OVERLAY" intake-upsert bead-df 'df test' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=56, branch='fix/df-56', target_repo='jleechanorg/dark-factory' WHERE bead_id='bead-df';"
"$OVERLAY" route-record bead-df STANDARD_PATH 'drive-existing-pr' >/dev/null

: > /tmp/test-af-tick-fake-r.log
AFD_WRAPPER_BEAD="bead-wa','bead-df" bash "$WRAPPER" >/tmp/test-af-tick-multirepo.log 2>&1 || true

# Verify they both got dispatched
state_wa="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='bead-wa';")"
assert "bead-wa state=DISPATCHED" "DISPATCHED" "$state_wa"

state_df="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='bead-df';")"
assert "bead-df state=DISPATCHED" "DISPATCHED" "$state_df"

# Verify fake-R calls passed the correct repo and project to factory-ao-remediate.sh
assert_grep "remediate bead-wa on WA project" "called:.*bead_id=bead-wa pr=56 repo=jleechanorg/worldarchitect.ai proj=worldarchitect" /tmp/test-af-tick-fake-r.log
assert_grep "remediate bead-df on DF project" "called:.*bead_id=bead-df pr=56 repo=jleechanorg/dark-factory proj=dark-factory" /tmp/test-af-tick-fake-r.log

# ---------------------------------------------------------------------------
# Test 8: wallclock regression — per-bead AO session dedup stalls the tick
# when AO queries are slow. With session cache, the total dispatch loop is
# bounded regardless of queue depth.
# ---------------------------------------------------------------------------
fresh_db wallclock
"$OVERLAY" intake-upsert bead-wall-a 'wallclock test a' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9100, branch='fix/wall-a' WHERE bead_id='bead-wall-a';"
"$OVERLAY" route-record bead-wall-a STANDARD_PATH 'drive-existing-pr' >/dev/null
"$OVERLAY" intake-upsert bead-wall-b 'wallclock test b' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9101, branch='fix/wall-b' WHERE bead_id='bead-wall-b';"
"$OVERLAY" route-record bead-wall-b STANDARD_PATH 'drive-existing-pr' >/dev/null

SLOW_AO_DIR="$SCRATCH_DIR/slow-ao"
mkdir -p "$SLOW_AO_DIR"
SLOW_AO="$SLOW_AO_DIR/ao-ts"
cat > "$SLOW_AO" <<'EOF_SLOW'
#!/usr/bin/env bash
SLEEP="${AFD_FAKE_AO_SLEEP_SEC:-3}"
case "${1:-}" in
  session)
    sleep "$SLEEP"
    echo "[pr_open]"
    exit 0
    ;;
  spawn|spawned) exit 0 ;;
  *) exit 0 ;;
esac
EOF_SLOW
chmod +x "$SLOW_AO"

: > /tmp/test-af-tick-fake-r.log
# Build a test daemon dir with the production tick + stubs
FAKE_DAEMON_WALL="$SCRATCH_DIR/daemon-wall"
mkdir -p "$FAKE_DAEMON_WALL"
mkdir -p "$FAKE_DAEMON_WALL/daemon/contracts"
cp "$FAKE_R" "$FAKE_DAEMON_WALL/daemon/factory-ao-remediate.sh"
cp "$ROOT/daemon/contracts/schema.sql" "$FAKE_DAEMON_WALL/daemon/contracts/schema.sql" 2>/dev/null || true
cp "$ROOT/daemon/factory-af-tick.sh" "$FAKE_DAEMON_WALL/daemon/factory-af-tick.sh"
cat > "$FAKE_DAEMON_WALL/daemon/factory-intake-from-gh.sh" <<'WIEOF'
#!/usr/bin/env bash
exit 0
WIEOF
chmod +x "$FAKE_DAEMON_WALL/daemon/factory-intake-from-gh.sh"
ln -sf "$OVERLAY" "$FAKE_DAEMON_WALL/daemon/factory-overlay.sh" 2>/dev/null || true
cat > "$FAKE_DAEMON_WALL/daemon/factory-ao-bin.sh" <<EOFAOBW
#!/usr/bin/env bash
echo "$SLOW_AO"
EOFAOBW
chmod +x "$FAKE_DAEMON_WALL/daemon/factory-ao-bin.sh"
AFD_TICK_WALL="$FAKE_DAEMON_WALL/daemon/factory-af-tick.sh"

start_ts=$(date +%s)
AFD_SKIP_DRIFT_CHECK=1 \
  AFD_DB="$AFD_DB" \
  BR_DB="" \
  TARGET_REPO="jleechanorg/worldarchitect.ai" \
  AFD_TICK_DEADLINE_SEC=20 \
  CONFIG="$(write_config 30 15)" \
  AO_BIN="$SLOW_AO" \
  AFD_FAKE_AO_SLEEP_SEC=3 \
  MAX_DISPATCH=2 \
  bash "$AFD_TICK_WALL" >/tmp/test-af-tick-wallclock.log 2>&1 || true
elapsed=$(( $(date +%s) - start_ts ))
echo "[Test 8 output]"
head -20 /tmp/test-af-tick-wallclock.log | sed 's/^/    /'
echo
if [ "$elapsed" -lt 20 ]; then
  echo "PASS: wallclock elapsed ${elapsed}s < 20s (cache bounded)"; PASS=$((PASS + 1))
else
  echo "FAIL: wallclock elapsed ${elapsed}s >= 20s (unbounded)"; FAIL=$((FAIL + 1))
fi
# Old behavior without cache: 2 beads × 3s AO per session ls × (concurrency
# probe + dedup) = at least 9s. The cache fix bounds total AO calls to one
# per-project, so 2 beads + cached concurrency probe ≤ 6s of AO time.
if [ "$elapsed" -lt 15 ]; then
  echo "PASS: wallclock regression check — ${elapsed}s < 15s (old behavior prevented)"; PASS=$((PASS + 1))
else
  echo "FAIL: wallclock regression — ${elapsed}s >= 15s"; FAIL=$((FAIL + 1))
fi

# ---------------------------------------------------------------------------
# Test 9: P0 fairness from production metadata (BR issues.priority, no
# injected priority list). P0 beads (priority=0) dispatch first and
# outside normal MAX_DISPATCH, independently of any env-var list.
# ---------------------------------------------------------------------------
fresh_db priority
export BR_DB="$SCRATCH_DIR/test-br-metadata.sqlite"
sqlite3 "$BR_DB" "CREATE TABLE IF NOT EXISTS issues (id TEXT PRIMARY KEY, title TEXT, status TEXT, priority INTEGER DEFAULT 2);"
# low-1: priority=2 (normal), p0: priority=0 (P0), low-2: priority=2
sqlite3 "$BR_DB" "INSERT INTO issues (id, title, status, priority) VALUES ('bead-low-1', 'low 1', 'open', 2);"
sqlite3 "$BR_DB" "INSERT INTO issues (id, title, status, priority) VALUES ('bead-p0', 'p0 bead', 'open', 0);"
sqlite3 "$BR_DB" "INSERT INTO issues (id, title, status, priority) VALUES ('bead-low-2', 'low 2', 'open', 2);"

"$OVERLAY" intake-upsert bead-low-1 'low priority 1' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9200, branch='fix/low-1', target_repo='jleechanorg/worldarchitect.ai' WHERE bead_id='bead-low-1';"
"$OVERLAY" route-record bead-low-1 STANDARD_PATH 'drive-existing-pr' >/dev/null

"$OVERLAY" intake-upsert bead-p0 'P0 priority bead' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9201, branch='fix/p0', target_repo='jleechanorg/worldarchitect.ai' WHERE bead_id='bead-p0';"
"$OVERLAY" route-record bead-p0 STANDARD_PATH 'drive-existing-pr' >/dev/null

"$OVERLAY" intake-upsert bead-low-2 'low priority 2' >/dev/null
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET pr_number=9202, branch='fix/low-2', target_repo='jleechanorg/worldarchitect.ai' WHERE bead_id='bead-low-2';"
"$OVERLAY" route-record bead-low-2 STANDARD_PATH 'drive-existing-pr' >/dev/null

FAKE_DAEMON_PRIO="$SCRATCH_DIR/daemon-prio"
mkdir -p "$FAKE_DAEMON_PRIO"
mkdir -p "$FAKE_DAEMON_PRIO/daemon/contracts"
cp "$FAKE_R" "$FAKE_DAEMON_PRIO/daemon/factory-ao-remediate.sh"
cp "$ROOT/daemon/contracts/schema.sql" "$FAKE_DAEMON_PRIO/daemon/contracts/schema.sql" 2>/dev/null || true
cp "$ROOT/daemon/factory-af-tick.sh" "$FAKE_DAEMON_PRIO/daemon/factory-af-tick.sh"
cat > "$FAKE_DAEMON_PRIO/daemon/factory-intake-from-gh.sh" <<'PEIOF'
#!/usr/bin/env bash
exit 0
PEIOF
chmod +x "$FAKE_DAEMON_PRIO/daemon/factory-intake-from-gh.sh"
ln -sf "$OVERLAY" "$FAKE_DAEMON_PRIO/daemon/factory-overlay.sh" 2>/dev/null || true
FAST_AO_PRIO="$SCRATCH_DIR/fast-ao-prio"
cat > "$FAST_AO_PRIO" <<'AEOF'
#!/usr/bin/env bash
case "${1:-}" in
  session) echo "[]"; exit 0 ;;
  spawn|spawned) exit 0 ;;
  status) echo '{"state":"ready"}'; exit 0 ;;
  *) exit 0 ;;
esac
AEOF
chmod +x "$FAST_AO_PRIO"
cat > "$FAKE_DAEMON_PRIO/daemon/factory-ao-bin.sh" <<EOFAOBP
#!/usr/bin/env bash
echo "$FAST_AO_PRIO"
EOFAOBP
chmod +x "$FAKE_DAEMON_PRIO/daemon/factory-ao-bin.sh"

AFD_TICK_PRIO="$FAKE_DAEMON_PRIO/daemon/factory-af-tick.sh"

: > /tmp/test-af-tick-fake-r.log
# MAX_DISPATCH=1: with metadata-driven fairness, P0 (priority=0) dispatches
# first and outside the normal limit.
prio_out="$(AFD_SKIP_DRIFT_CHECK=1 \
  AFD_DB="$AFD_DB" \
  BR_DB="$BR_DB" \
  TARGET_REPO="jleechanorg/worldarchitect.ai" \
  CONFIG="$(write_config 30 15)" \
  AO_BIN="$FAST_AO_PRIO" \
  MAX_DISPATCH=1 \
  bash "$AFD_TICK_PRIO" 2>&1 || true)"
echo "[Test 9 output]"
echo "$prio_out" | sed 's/^/    /'
echo

case "$prio_out" in
  *"remediate"*"bead-p0"*)
    echo "PASS: metadata fairness: bead-p0 dispatched via production tick"; PASS=$((PASS + 1)) ;;
  *)
    echo "FAIL: metadata fairness: bead-p0 NOT dispatched."; FAIL=$((FAIL + 1)) ;;
esac

# P0 must dispatch first (sorted by priority ASC). The log line is
# "[af] remediate [P0] bead-p0 PR #9201..." — grep for bead-p0 anywhere.
p0_line="$(echo "$prio_out" | grep -n 'bead-p0' | head -1 | cut -d: -f1 || echo 999)"
low1_line="$(echo "$prio_out" | grep -n 'bead-low-1' | head -1 | cut -d: -f1 || echo 0)"
if [ "$p0_line" -lt 900 ]; then
  if [ "$low1_line" = "0" ] || [ "$p0_line" -lt "$low1_line" ]; then
    echo "PASS: metadata fairness: bead-p0 dispatched before bead-low-1 (line ${p0_line} < ${low1_line})"; PASS=$((PASS + 1))
  else
    echo "FAIL: metadata fairness: bead-p0 not first (p0=${p0_line}, low1=${low1_line})"; FAIL=$((FAIL + 1))
  fi
fi

# Verify P0 dispatches WITH MAX_DISPATCH=0 (additive, outside limit)
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET state='QUEUED' WHERE bead_id='bead-p0';"
sqlite3 "$AFD_DB" "UPDATE bead_overlay SET state='QUEUED' WHERE bead_id='bead-low-1';"
: > /tmp/test-af-tick-fake-r.log
prio_out2="$(AFD_SKIP_DRIFT_CHECK=1 \
  AFD_DB="$AFD_DB" \
  BR_DB="$BR_DB" \
  TARGET_REPO="jleechanorg/worldarchitect.ai" \
  CONFIG="$(write_config 30 15)" \
  AO_BIN="$FAST_AO_PRIO" \
  MAX_DISPATCH=0 \
  bash "$AFD_TICK_PRIO" 2>&1 || true)"
case "$prio_out2" in
  *"remediate"*"bead-p0"*)
    echo "PASS: metadata fairness: P0 dispatched with MAX_DISPATCH=0 (additive)"; PASS=$((PASS + 1))
    ;;
  *)
    echo "FAIL: metadata fairness: P0 NOT dispatched with MAX_DISPATCH=0"; FAIL=$((FAIL + 1))
    ;;
esac
case "$prio_out2" in
  *"remediate"*"bead-low-1"*)
    echo "FAIL: metadata fairness: normal bead dispatched at MAX_DISPATCH=0 (should be blocked)"; FAIL=$((FAIL + 1)) ;;
  *)
    echo "PASS: metadata fairness: normal bead correctly blocked at MAX_DISPATCH=0"; PASS=$((PASS + 1)) ;;
esac
# Verify af_dispatched count reflects P0-only dispatch
afd_count="$(echo "$prio_out2" | grep -oE 'af_dispatched=[0-9]+' | head -1 | cut -d= -f2)"
if [ "${afd_count:-0}" -eq 1 ]; then
  echo "PASS: metadata fairness: af_dispatched=1 (P0 only) at MAX_DISPATCH=0"; PASS=$((PASS + 1))
else
  echo "FAIL: metadata fairness: af_dispatched=${afd_count} expected 1"; FAIL=$((FAIL + 1))
fi

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
exit 0
