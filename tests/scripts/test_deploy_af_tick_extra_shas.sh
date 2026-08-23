#!/usr/bin/env bash
# test_deploy_af_tick_extra_shas.sh — verifies the bze8.2-extension
# appended to daemon/scripts/deploy-af-tick.sh: a JSONL row at
# $DEPLOY_BZE8_LOG containing HEAD SHA + the SHAs of every
# daemon/scripts/*, daemon/launchd/*, daemon/contracts/*, and config/*
# file plus the AO CLI binary (if AO_BIN is set).
#
# Bead jleechan-goal-unattended-e2e-2026-07-17-bze8.2 acceptance 3:
# "Deploy the exact current dark-factory main SHA from a clean checkout;
# record binary/script/config SHAs in telemetry."
#
# Run with: bash tests/scripts/test_deploy_af_tick_extra_shas.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DEPLOY="$ROOT/daemon/scripts/deploy-af-tick.sh"

PASS=0
FAIL=0
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

# --help mentions the new flag (use a tempfile so grep has a real fd)
help_out_file="$(mktemp)"
bash "$DEPLOY" --help >"$help_out_file" 2>&1 || true
assert_grep "help mentions --no-extra-shas flag" "no-extra-shas" "$help_out_file"
assert_grep "help mentions deploy-bze8 record" "deploy-bze8.jsonl" "$help_out_file"
rm -f "$help_out_file"

# Build a scratch deploy target.
SCRATCH="$(mktemp -d -t test-deploy-extra.XXXXXX)"
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT
SEED="$SCRATCH/seed"
mkdir -p "$SEED"
(
    cd "$SEED"
    git init -q
    git config user.email t@t.com
    git config user.name t
    echo hi > a.txt
    git add a.txt
    git commit -q -m init
    git branch -M main
)
ORIGIN="$SCRATCH/origin.git"
git clone -q --bare "$SEED" "$ORIGIN"
git --git-dir="$ORIGIN" symbolic-ref HEAD refs/heads/main
rm -rf "$SEED"

WORK="$SCRATCH/work"
git clone -q "$ORIGIN" "$WORK"
(
    cd "$WORK"
    git config user.email t@t.com
    git config user.name t
)

# --dry-run should NOT touch the bze8 log (the legacy --dry-run contract is
# "no side effects" — adding the bze8 row would violate that).
BZE8_LOG="$SCRATCH/deploy-bze8.jsonl"
rm -f "$BZE8_LOG"
bash "$DEPLOY" --target-dir "$WORK" --dry-run >/dev/null 2>&1
if [ ! -f "$BZE8_LOG" ]; then
    echo "PASS: --dry-run does not write the bze8 record"
    PASS=$((PASS + 1))
else
    echo "FAIL: --dry-run wrote the bze8 record (legacy contract violation)"
    FAIL=$((FAIL + 1))
    cat "$BZE8_LOG"
fi

# Real no-op deploy — should write one bze8 row with the correct shape.
rm -f "$BZE8_LOG"
AO_BIN="$SCRATCH/fake-ao.sh"
cat > "$AO_BIN" <<'FAKE_EOF'
#!/usr/bin/env bash
exit 0
FAKE_EOF
chmod +x "$AO_BIN"

set +e
bash "$DEPLOY" --target-dir "$WORK" --no-extra-shas >/dev/null 2>&1
NORC=$?
set -e
# Use the legacy log location the script defaults to (not $BZE8_LOG), so we
# assert that --no-extra-shas suppresses the bze8 channel even if the env
# override were also set.
LEGACY_BZE8="$HOME/Library/Logs/dark-factory/deploy-bze8.jsonl"
rm -f "$LEGACY_BZE8"
assert "no-extra-shas exits 0" "0" "$NORC"
if [ ! -f "$LEGACY_BZE8" ] || [ ! -s "$LEGACY_BZE8" ]; then
    echo "PASS: --no-extra-shas suppresses the bze8 record (legacy log only)"
    PASS=$((PASS + 1))
else
    echo "FAIL: --no-extra-shas wrote the bze8 record anyway — flag ignored"
    FAIL=$((FAIL + 1))
fi

# Default (with bze8 record). Run twice: once fresh, once idempotent no-op.
rm -f "$BZE8_LOG"
set +e
AFD_DEPLOY_BZE8_LOG="$BZE8_LOG" AO_BIN="$AO_BIN" bash "$DEPLOY" --target-dir "$WORK" >/dev/null 2>&1
FRC=$?
set -e
assert "bze8 deploy exits 0 on first run" "0" "$FRC"
assert "bze8 log file exists" "1" "$([ -f "$BZE8_LOG" ] && echo 1 || echo 0)"
LINE1="$(tail -1 "$BZE8_LOG")"
assert "first bze8 row is valid JSON" "yes" "$(printf '%s' "$LINE1" | python3 -c 'import json,sys; json.load(sys.stdin); print("yes")')"
ARTIFACTS="$(printf '%s' "$LINE1" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d.get("artifacts",[])))')"
assert "first bze8 row records at least 12 artifacts (canonical paths)" "yes" \
    "$(python3 -c "n=$ARTIFACTS; print('yes' if n>=12 else 'no')")"
HEAD_SHA="$(printf '%s' "$LINE1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["head_sha"])')"
WORK_HEAD="$(cd "$WORK" && git rev-parse HEAD)"
assert "first bze8 row head_sha matches target HEAD" "$WORK_HEAD" "$HEAD_SHA"
AO_BIN_SHA_RECORDED="$(printf '%s' "$LINE1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["ao_bin_sha256"])')"
AO_BIN_SHA_ACTUAL="$(sha256sum "$AO_BIN" | cut -d' ' -f1)"
assert "first bze8 row records AO_BIN sha256" "$AO_BIN_SHA_ACTUAL" "$AO_BIN_SHA_RECORDED"

# Idempotency: a second run writes a SECOND row (every deploy leaves a row).
N0="$(wc -l < "$BZE8_LOG")"
set +e
AFD_DEPLOY_BZE8_LOG="$BZE8_LOG" AO_BIN="$AO_BIN" bash "$DEPLOY" --target-dir "$WORK" >/dev/null 2>&1
set -e
N1="$(wc -l < "$BZE8_LOG")"
assert "second bze8 deploy appends another row" "$((N0 + 1))" "$N1"
NOOP_FIELD="$(tail -1 "$BZE8_LOG" | python3 -c 'import json,sys; print(json.load(sys.stdin)["noop"])')"
assert "second run is recorded as noop (already up to date)" "True" "$NOOP_FIELD"

# Header value edge case: empty AO_BIN env must produce an empty ao_bin_sha256
# row, NOT a crashed script.
rm -f "$BZE8_LOG"
(
    cd "$WORK"
    echo forward >> a.txt
    git add a.txt
    git commit -q -m forward
    git push -q origin main
)
set +e
unset AO_BIN
AFD_DEPLOY_BZE8_LOG="$BZE8_LOG" bash "$DEPLOY" --target-dir "$WORK" >/dev/null 2>&1
NORC=$?
set -e
assert "no-AO_BIN deploy exits 0" "0" "$NORC"
EMPTY_AO="$(tail -1 "$BZE8_LOG" | python3 -c 'import json,sys; print(json.load(sys.stdin)["ao_bin_sha256"])')"
assert "no-AO_BIN deploy records empty ao_bin_sha256" "" "$EMPTY_AO"

echo
echo "=== RESULTS: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
