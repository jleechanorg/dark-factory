#!/usr/bin/env bash
# test_bze8_2_canary.sh — CI gate for scripts/canary/bze8.2-canary.sh.
#
# Bead jleechan-goal-unattended-e2e-2026-07-17-bze8.2 / acceptance 6: an
# end-to-end Linux-runnable canary that proves the contracts the AF tick
# and overlay enforce (multi-repo dispatch, fail-closed AO probe, no-bulk
# stale recovery, deploy SHA record, full QUEUED → READY lifecycle).
#
# The canary script requires:
#   * sqlite3, python3, cargo (for the first run only — builds rust daemon)
#   * AF_TICK / overlay shells present on the repo (always true in this checkout)
#
# Run with: bash tests/scripts/test_bze8_2_canary.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CANARY="$ROOT/scripts/canary/bze8.2-canary.sh"

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

# 1. The canary script exists and is executable.
[ -x "$CANARY" ] && { echo "PASS: bze8.2-canary.sh is executable"; PASS=$((PASS+1)); } \
    || { echo "FAIL: bze8.2-canary.sh missing or not executable"; FAIL=$((FAIL+1)); }

# 2. --help exits with the docs the operator needs (no actual execution).
HELP_FILE="$(mktemp)"
bash "$CANARY" --help >"$HELP_FILE" 2>&1 || true
assert_grep "canary --help mentions bead flag" "bead" "$HELP_FILE"
assert_grep "canary --help mentions out-dir" "out-dir" "$HELP_FILE"
assert_grep "canary --help mentions report-only" "report" "$HELP_FILE"

# 3. Run the canary against a fresh scratch dir. Skip if no cargo / sqlite3.
SCRATCH="$(mktemp -d -t bze8-test.XXXXXX)"
cleanup() { rm -rf "$SCRATCH" "$HELP_FILE"; }
trap cleanup EXIT

# The canary E2E needs BOTH the sqlite3 CLI and a Rust toolchain: it builds
# `daemon/target/debug/daemon` before exercising the tick. This mirrors the
# pre-existing sqlite3 guard rather than inventing a new convention.
#
# Bead jleechan-kn5j: `cargo` is absent in the `test` job — only `daemon-tests`
# installs a Rust toolchain — so on Linux runners the canary died with
# "line 171: cargo: command not found / daemon build failed" and, under set -e,
# never wrote its evidence bundle. That surfaced as five artifact assertions
# failing for what looked like unrelated reasons.
#
# Skipping is honest here (the job genuinely cannot run this E2E) and the
# canary IS still covered: `daemon-tests`, which has the toolchain, is the
# right home for it. Reported as SKIP, never as PASS, so the gap stays visible.
if ! command -v sqlite3 >/dev/null 2>&1; then
    echo "SKIP: sqlite3 not installed; canary E2E test cannot run"
elif ! command -v cargo >/dev/null 2>&1; then
    echo "SKIP: cargo not on PATH; canary E2E builds the rust daemon and cannot run"
else
    set +e
    # Bead jleechan-kn5j: capture instead of discarding. Sending the canary's
    # output to /dev/null means a CI failure surfaces as a bare exit code with
    # no cause — this exact test failed on Linux runners for hours showing only
    # "canary exits 0 overall (expected '0', got '1')" while the real error
    # ("no such table: bead_overlay") was thrown away. Keep it quiet on success,
    # print it on failure.
    CANARY_LOG="$SCRATCH/canary-run.log"
    bash "$CANARY" --out-dir "$SCRATCH" >"$CANARY_LOG" 2>&1
    CANARY_RC=$?
    set -e
    if [ "$CANARY_RC" -ne 0 ]; then
        echo "--- canary output (rc=$CANARY_RC) ---"
        tail -40 "$CANARY_LOG" | sed 's/^/    /'
        echo "--- end canary output ---"
    fi

    # Crucial: the canary itself exits 0. (An internal AF-tick rc=1 is OK —
    # that's acceptance-2 fail-closed in action — as long as the CANARY exits
    # 0 because every gate-emission check inside the script passed.)
    assert "canary exits 0 overall" "0" "$CANARY_RC"

    # REPORT.json exists with all 9 canonical event types AND terminal_state=READY.
    REPORT="$SCRATCH/REPORT.json"
    assert "REPORT.json exists" "1" "$([ -f "$REPORT" ] && echo 1 || echo 0)"
    if [ -f "$REPORT" ]; then
        TERMINAL="$(python3 -c "import json; print(json.load(open('$REPORT'))['terminal_state'])" 2>/dev/null || echo unknown)"
        assert "REPORT.terminal_state == READY" "READY" "$TERMINAL"
        EVENT_TYPES="$(python3 -c "
import json
d=json.load(open('$REPORT'))
print(' '.join(sorted({ev['event_type'] for ev in d.get('events',[])})))" 2>/dev/null)"
        assert_grep "REPORT contains multi-repo resolution event" "multirepo.resolution" <(printf '%s' "$EVENT_TYPES")
        assert_grep "REPORT contains stale recovery event" "stale.reconciled" <(printf '%s' "$EVENT_TYPES")
        assert_grep "REPORT contains af-tick first-run event" "af_tick.first_run" <(printf '%s' "$EVENT_TYPES")
        assert_grep "REPORT contains broken-AO probe event" "af_tick.bad_ao" <(printf '%s' "$EVENT_TYPES")
        assert_grep "REPORT contains canary lifecycled event" "canary.lifecycled" <(printf '%s' "$EVENT_TYPES")
        assert_grep "REPORT contains deploy recorded event" "deploy.recorded" <(printf '%s' "$EVENT_TYPES")
    fi

    # EVIDENCE.md surfaces the 7-acceptance matrix
    EVIDENCE="$SCRATCH/EVIDENCE.md"
    assert "EVIDENCE.md exists" "1" "$([ -f "$EVIDENCE" ] && echo 1 || echo 0)"
    if [ -f "$EVIDENCE" ]; then
        assert_grep "EVIDENCE lists AO-restore acceptance" "AO TS CLI restore" "$EVIDENCE"
        assert_grep "EVIDENCE lists bounded+fail-closed acceptance" "Bounded .* fail-closed" "$EVIDENCE"
        assert_grep "EVIDENCE lists deploy+SHAs acceptance" "Deploy .* SHAs" "$EVIDENCE"
        assert_grep "EVIDENCE lists multi-repo routing acceptance" "Multi-repo routing" "$EVIDENCE"
        assert_grep "EVIDENCE lists stale-recovery acceptance" "Stale recovery" "$EVIDENCE"
        assert_grep "EVIDENCE lists canary E2E acceptance" "Canary E2E" "$EVIDENCE"
        assert_grep "EVIDENCE lists evidence-bundle acceptance" "Evidence bundle" "$EVIDENCE"
    fi

    # deploy-bze8.jsonl records the canonical SHAs
    DEPLOY_REC="$SCRATCH/cxdb/deploy-bze8.jsonl"
    assert "deploy-bze8.jsonl exists" "1" "$([ -f "$DEPLOY_REC" ] && echo 1 || echo 0)"
    if [ -f "$DEPLOY_REC" ]; then
        # Read JSONL with python; filter empty lines; count artifacts in the
        # first non-empty line so a malformed canary record surfaces
        # immediately (we still exit the test even if 0, to make the
        # regression loud).
        ARTIFACT_COUNT="$(python3 <<PY 2>/dev/null || echo 0
import json
with open("$DEPLOY_REC") as fh:
    for line in fh:
        line = line.strip()
        if not line: continue
        d = json.loads(line)
        print(len(d.get('artifacts', [])))
        break
PY
)"
        assert "deploy-bze8 record has 12+ artifacts" "yes" \
            "$(python3 -c "n=int('$ARTIFACT_COUNT'); print('yes' if n>=12 else 'no')")"
    fi

    # Final state assertions on the CXDB itself
    DB="$SCRATCH/cxdb/daemon.sqlite"
    assert "CXDB has READY bead row" "1" "$(sqlite3 "$DB" "
SELECT count(*) FROM bead_overlay WHERE state='READY' AND bead_id='bze8.2-canary-1';")"
    # After stale-recovery we should see 56 QUEUED + 8 stale DISPATCHED
    # (untouched because they have no spawn-state file).
    POST_DISPATCHED="$(sqlite3 "$DB" "SELECT count(*) FROM bead_overlay WHERE state='DISPATCHED';")"
    assert "CXDB post-recovery: 8 DISPATCHED rows left untouched" "8" "$POST_DISPATCHED"
fi

# 4. canary --help from a directory containing a single space in out-dir
# also works (regression check on the bash strict-mode arg parsing).
SPACED="$SCRATCH/with space"
mkdir -p "$SPACED"
set +e
bash "$CANARY" --out-dir "$SPACED" >/dev/null 2>&1
SP_RC=$?
set -e
assert "canary accepts --out-dir containing spaces" "0" "$SP_RC"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
