#!/usr/bin/env bash
# test_factory_overlay_branch_scope.sh — round-trip tests for the
# warn_branch_scope_mismatch gate in factory-overlay.sh:dispatch-record
# (bead ez-gh-actions-oxog — PR-scope-mismatch prevention).
#
# Verifies:
#   1. dispatch-record still succeeds (rc=0) when the branch name matches
#      a scope-defining stem in the bead title (no false-positive warn).
#   2. dispatch-record still succeeds (rc=0) when the bead title has NO
#      scope-defining stems (e.g. a short generic title) and the branch is
#      named after the bead_id — generic branch names are legitimate, the
#      gate is warn-only.
#   3. dispatch-record still succeeds (rc=0) but emits the [oxog WARN] line
#      to stderr when the bead title contains scope-stems (e.g. "converge
#      lima namespace") and the branch name contains NONE of them — the
#      warn is the signal the verifier spot-checks.
#   4. The function is fail-safe: when `br show` is missing / broken, no
#      warn is emitted (a missing signal must NEVER block dispatch).
#   5. State still transitions QUEUED → DISPATCHED in all cases — the
#      gate is a warn, not a hard fail.
#
# Run with: bash tests/scripts/test_factory_overlay_branch_scope.sh
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
assert_not_grep() {
  local name="$1" pattern="$2" file="$3"
  if ! grep -qE "$pattern" "$file" 2>/dev/null; then
    echo "PASS: $name"
    PASS=$((PASS + 1))
  else
    echo "FAIL: $name (pattern '$pattern' unexpectedly found in $file)"
    FAIL=$((FAIL + 1))
  fi
}

# Scratch dir (per-test re-created so each test starts with a clean slate).
SCRATCH_DIR="$(mktemp -d -t test-branch-scope.XXXXXX)"
cleanup() { rm -rf "$SCRATCH_DIR"; }
trap cleanup EXIT

# Helper: fresh DB + log + apply schema; sets AFD_DB / AFD_LOG accordingly.
fresh_db() {
  local tag="${1:-main}"
  export AFD_DB="$SCRATCH_DIR/cxdb-$tag.sqlite"
  export AFD_LOG="$SCRATCH_DIR/cxdb-$tag.jsonl"
  "$OVERLAY" init >/dev/null
}

# Config with default capacity.
write_config() {
  local mw="${1:-30}" mb="${2:-15}"
  local cfg="$SCRATCH_DIR/daemon-cap-$mw-$mb.toml"
  cat > "$cfg" <<TOML_EOF
max_workers = $mw
max_batch = $mb
TOML_EOF
  printf '%s' "$cfg"
}

# Fake `br` shim whose bead titles are controllable via files in SCRATCH_DIR.
# bead titles live at $SCRATCH_DIR/bead-<id>.title — one per bead.
FAKE_BR="$SCRATCH_DIR/br"
cat > "$FAKE_BR" <<'BR_EOF'
#!/usr/bin/env bash
# Fake br shim: returns title from $FAKE_BR_DIR/bead-<id>.title, else ""
case "${1:-}" in
  show)
    bead="$2"
    fmt="${3:-}"
    if [ "$fmt" = "--format" ] || [ "$fmt" = "--json" ]; then
      title="$(cat "${FAKE_BR_DIR:-/tmp}/bead-${bead}.title" 2>/dev/null || true )"
      printf '[{"id":"%s","title":"%s","status":"open"}]\n' "$bead" "$title"
    fi
    ;;
esac
BR_EOF
chmod +x "$FAKE_BR"
export FAKE_BR_DIR="$SCRATCH_DIR"
export BR_BIN="$FAKE_BR"

# ---------------------------------------------------------------------------
# Test 1: title has scope-stems AND branch contains one of them — no warn
# (branch correctly reflects the diff scope)
# ---------------------------------------------------------------------------
fresh_db t1
export CONFIG="$(write_config 30 15)"
echo "Converge lima dual namespace to one backend" > "$FAKE_BR_DIR/bead-oxog-scope-match.title"
"$OVERLAY" intake-upsert oxog-scope-match 'Converge lima dual namespace to one backend' >/dev/null
"$OVERLAY" route-record oxog-scope-match STANDARD_PATH 'fix' >/dev/null
ERR_FILE="$SCRATCH_DIR/err-t1"
set +e
"$OVERLAY" dispatch-record oxog-scope-match fix/lima-converge 2>"$ERR_FILE" >/dev/null
rc=$?
set -e
assert "Test1 rc=0 (scope-matched branch)" "0" "$rc"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='oxog-scope-match';")"
assert "Test1 state=DISPATCHED" "DISPATCHED" "$state"
assert_not_grep "Test1 no [oxog WARN] when scope matches" '\[oxog WARN\]' "$ERR_FILE"

# ---------------------------------------------------------------------------
# Test 2: title has NO scope-stems AND branch is generic — no warn
# (generic branch names are legitimate when title is already descriptive)
# ---------------------------------------------------------------------------
fresh_db t2
echo "Add a new feature" > "$FAKE_BR_DIR/bead-oxog-generic.title"
"$OVERLAY" intake-upsert oxog-generic 'Add a new feature' >/dev/null
"$OVERLAY" route-record oxog-generic STANDARD_PATH 'fix' >/dev/null
ERR_FILE="$SCRATCH_DIR/err-t2"
set +e
"$OVERLAY" dispatch-record oxog-generic fix/oxog-generic 2>"$ERR_FILE" >/dev/null
rc=$?
set -e
assert "Test2 rc=0 (generic branch, generic title)" "0" "$rc"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='oxog-generic';")"
assert "Test2 state=DISPATCHED" "DISPATCHED" "$state"
assert_not_grep "Test2 no [oxog WARN] when title has no scope-stems" '\[oxog WARN\]' "$ERR_FILE"

# ---------------------------------------------------------------------------
# Test 3: title has scope-stems AND branch has NONE — WARN emitted, dispatch
# still succeeds (bead ez-gh-actions-oxog live pattern: bead about lima
# convergence shipped as deadline-clamp branch with no scope-stem match)
# ---------------------------------------------------------------------------
fresh_db t3
echo "Converge lima dual namespace to one backend" > "$FAKE_BR_DIR/bead-oxog-mismatch.title"
"$OVERLAY" intake-upsert oxog-mismatch 'Converge lima dual namespace to one backend' >/dev/null
"$OVERLAY" route-record oxog-mismatch STANDARD_PATH 'fix' >/dev/null
ERR_FILE="$SCRATCH_DIR/err-t3"
set +e
"$OVERLAY" dispatch-record oxog-mismatch fix/oxog-mismatch 2>"$ERR_FILE" >/dev/null
rc=$?
set -e
assert "Test3 rc=0 (warn is warn, not fail)" "0" "$rc"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='oxog-mismatch';")"
assert "Test3 state=DISPATCHED (warn did not block)" "DISPATCHED" "$state"
assert_grep "Test3 [oxog WARN] emitted" '\[oxog WARN\]' "$ERR_FILE"
assert_grep "Test3 [oxog WARN] names the bead" 'bead=oxog-mismatch' "$ERR_FILE"
assert_grep "Test3 [oxog WARN] references the bead id" 'ez-gh-actions-oxog' "$ERR_FILE"
assert_grep "Test3 [oxog WARN] names the branch" "branch='fix/oxog-mismatch'" "$ERR_FILE"

# ---------------------------------------------------------------------------
# Test 4: title has scope-stems (timeout/deadline) and branch has a stem
# that substring-matches one of them (e.g. `clamp` matches `clamped`) — no warn
# ---------------------------------------------------------------------------
fresh_db t4
echo "Backend restart timeout deadline propagation" > "$FAKE_BR_DIR/bead-oxog-stem.title"
"$OVERLAY" intake-upsert oxog-stem 'Backend restart timeout deadline propagation' >/dev/null
"$OVERLAY" route-record oxog-stem STANDARD_PATH 'fix' >/dev/null
ERR_FILE="$SCRATCH_DIR/err-t4"
set +e
"$OVERLAY" dispatch-record oxog-stem fix/clamped-timeout 2>"$ERR_FILE" >/dev/null
rc=$?
set -e
assert "Test4 rc=0 (substring stem match)" "0" "$rc"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='oxog-stem';")"
assert "Test4 state=DISPATCHED" "DISPATCHED" "$state"
assert_not_grep "Test4 no [oxog WARN] (substring match)" '\[oxog WARN\]' "$ERR_FILE"

# ---------------------------------------------------------------------------
# Test 5: bead_id is a valid beaid in CXDB but `br` lookup yields no title —
# dispatch still succeeds, no warn (fail-safe: missing signal must never block)
# ---------------------------------------------------------------------------
fresh_db t5
"$OVERLAY" intake-upsert oxog-no-signal 'whatever' >/dev/null
"$OVERLAY" route-record oxog-no-signal STANDARD_PATH 'fix' >/dev/null
# Wipe the br shim's title file so br returns an empty title.
rm -f "$FAKE_BR_DIR/bead-oxog-no-signal.title"
ERR_FILE="$SCRATCH_DIR/err-t5"
set +e
"$OVERLAY" dispatch-record oxog-no-signal fix/oxog-no-signal 2>"$ERR_FILE" >/dev/null
rc=$?
set -e
assert "Test5 rc=0 (no br signal)" "0" "$rc"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='oxog-no-signal';")"
assert "Test5 state=DISPATCHED (no signal, no block)" "DISPATCHED" "$state"
assert_not_grep "Test5 no [oxog WARN] when no title" '\[oxog WARN\]' "$ERR_FILE"

# ---------------------------------------------------------------------------
# Test 6: title contains 'drain' and 'clamp' (the exact failure class from
# PR #56) and branch is `feat/oxog-9c7l` (matches bead id, no scope-stem) —
# [oxog WARN] fires; dispatch still succeeds
# ---------------------------------------------------------------------------
fresh_db t6
echo "Linux dual lima namespace after reboot: drain jobs, clamp timeout, converge to one backend" > "$FAKE_BR_DIR/bead-oxog-rq8u.title"
"$OVERLAY" intake-upsert oxog-rq8u 'Linux dual lima namespace after reboot: drain jobs, clamp timeout, converge to one backend' >/dev/null
"$OVERLAY" route-record oxog-rq8u STANDARD_PATH 'fix' >/dev/null
ERR_FILE="$SCRATCH_DIR/err-t6"
set +e
"$OVERLAY" dispatch-record oxog-rq8u feat/oxog-9c7l 2>"$ERR_FILE" >/dev/null
rc=$?
set -e
assert "Test6 rc=0 (PR #56 live pattern)" "0" "$rc"
state="$(sqlite3 "$AFD_DB" "SELECT state FROM bead_overlay WHERE bead_id='oxog-rq8u';")"
assert "Test6 state=DISPATCHED" "DISPATCHED" "$state"
assert_grep "Test6 [oxog WARN] on the PR #56 pattern" '\[oxog WARN\]' "$ERR_FILE"
# The warn should list at least one scope-stem from the title.
assert_grep "Test6 [oxog WARN] mentions lima" 'lima' "$ERR_FILE"
assert_grep "Test6 [oxog WARN] mentions drain" 'drain' "$ERR_FILE"
assert_grep "Test6 [oxog WARN] mentions clamp" 'clamp' "$ERR_FILE"
assert_grep "Test6 [oxog WARN] mentions converge" 'converge' "$ERR_FILE"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo "----------------------------------------"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
