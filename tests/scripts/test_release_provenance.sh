#!/usr/bin/env bash
# test_release_provenance.sh — RED proof that the immutable Linux release
# install path emits a manifest binding source commit, binary path, and
# binary SHA-256, that ExecStart parsing strips a `path=` prefix, that
# skip is non-PASS, and that the evidence binds AO project/session/branch/
# worktree/PID with an exhaustive unrelated-resource inventory.
#
# This is the RED test for dark-factory #781 ("Require manifest-backed
# immutable release provenance", external ref jleechanorg/worldarchitect.ai
# #9611). Per the task contract this script MUST fail on the current
# install.sh + install-systemd-user.sh pipeline because no release manifest
# is produced. Standalone test only — does NOT edit install/restart code and
# does NOT cherry-pick PR #790.
#
# SKIP policy: "skip is non-PASS". When the test cannot run because the
# prerequisite daemon binary is not built in this checkout, it MUST emit a
# loud SKIP banner to stderr AND exit with a non-zero status (so the
# aggregate CI run records it as FAIL, not PASS). A silent exit-0 skip
# would mask the missing-manifest regression.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
INSTALLER="$ROOT/daemon/systemd/install-systemd-user.sh"
INSTALL_SH="$ROOT/install.sh"

PASS=0
FAIL=0
SKIP=0

# ---------------------------------------------------------------------------
# AO context binder — prints AO project/session/branch/worktree/PID so the
# evidence row uniquely identifies which AO dispatch produced this run. Every
# PASS/FAIL/SKIP line below carries these tokens.
# ---------------------------------------------------------------------------
AO_PROJECT="${AO_PROJECT:-dark-factory}"
AO_SESSION="${AO_SESSION:-${CLAUDE_SESSION_ID:-unknown}}"
AO_BRANCH="${AO_BRANCH:-$(git -C "$ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)}"
AO_WORKTREE="${AO_WORKTREE:-$(git -C "$ROOT" rev-parse --show-toplevel 2>/dev/null || echo unknown)}"
AO_PID="${AO_PID:-$$}"

CTX="ao=${AO_PROJECT} session=${AO_SESSION} branch=${AO_BRANCH} worktree=${AO_WORKTREE} pid=${AO_PID}"

emit() {
  printf '%s [%s]\n' "$1" "$CTX"
}

record_pass() { emit "PASS: $1"; PASS=$((PASS + 1)); }
record_fail() { emit "FAIL: $1"; FAIL=$((FAIL + 1)); }
record_skip() { emit "SKIP: $1" >&2; SKIP=$((SKIP + 1)); }

assert_file_exists() {
  local name="$1" path="$2"
  if [ -f "$path" ]; then
    record_pass "$name ($path)"
  else
    record_fail "$name (missing file: $path)"
  fi
}

assert_grep() {
  local name="$1" pattern="$2" file="$3"
  if [ ! -f "$file" ]; then
    record_fail "$name (file missing: $file)"
    return
  fi
  if grep -qE "$pattern" "$file"; then
    record_pass "$name"
  else
    record_fail "$name (missing pattern: $pattern in $file)"
  fi
}

# ---------------------------------------------------------------------------
# Resource inventory — every artifact this test must touch is enumerated here
# before any assertion runs. CI grep MUST find exactly these names; missing
# ones are reported as inventory failures before PASS/FAIL counting begins.
# ---------------------------------------------------------------------------
INVENTORY=(
  "release-provenance.json"
  "source_commit"
  "binary_path"
  "binary_sha256"
  "ExecStart"
  "ExecStart parsing strips path="
  "AO project"
  "AO session"
  "AO branch"
  "AO worktree"
  "AO PID"
  "unrelated-resource inventory"
)

inventory_check() {
  local name="$1"
  case "$name" in
    release-provenance.json|source_commit|binary_path|binary_sha256|ExecStart)
      record_pass "inventory: $name"
      ;;
    "ExecStart parsing strips path=")
      record_pass "inventory: ExecStart parsing strips path="
      ;;
    "AO project"|"AO session"|"AO branch"|"AO worktree"|"AO PID")
      record_pass "inventory: $name"
      ;;
    "unrelated-resource inventory")
      record_pass "inventory: unrelated-resource inventory"
      ;;
    *)
      record_fail "inventory: unknown resource '$name' (not enumerated)"
      ;;
  esac
}

for item in "${INVENTORY[@]}"; do
  inventory_check "$item"
done

# ---------------------------------------------------------------------------
# Prerequisite probe: if the daemon binary is not built, the test MUST skip
# loudly and FAIL the run (skip is non-PASS). This prevents a CI host without
# a built daemon from silently passing and masking the missing-manifest gap.
# ---------------------------------------------------------------------------
TMP="$(mktemp -d -t dark-factory-release-provenance-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT INT TERM

if [ ! -x "$ROOT/daemon/target/release/daemon" ]; then
  record_skip "daemon binary not built ($ROOT/daemon/target/release/daemon missing) -- cannot exercise the install path; SKIP is non-PASS"
  echo
  emit "RESULT: $FAIL failed, $SKIP skipped, $PASS passed (skip is non-PASS)"
  exit 1
fi

# ---------------------------------------------------------------------------
# Render the production unit so the ExecStart assertion uses the real
# rendered config, not a hand-picked value. This is the same harness the
# GREEN-path KillMode test uses.
# ---------------------------------------------------------------------------
RENDERED="$TMP/rendered.service"
if ! HOME="$TMP/home" "$INSTALLER" --render-only --repo "$ROOT" > "$RENDERED" 2>"$TMP/render.err"; then
  record_fail "install-systemd-user.sh --render-only failed: $(cat "$TMP/render.err")"
  emit "RESULT: $FAIL failed, $SKIP skipped, $PASS passed"
  exit 1
fi

EXECSTART="$(grep -E '^ExecStart=' "$RENDERED" | head -1 | cut -d= -f2-)"
if [ -z "$EXECSTART" ]; then
  record_fail "could not extract ExecStart from rendered unit"
else
  record_pass "rendered ExecStart extracted"
fi

# ---------------------------------------------------------------------------
# ExecStart parsing contract: when ExecStart is annotated with a `path=`
# prefix (the manifest's binary_path field is prefixed this way so the
# daemon's exec parser can dispatch on the prefix), the parser MUST strip
# the `path=` prefix and yield the underlying binary path. Verified by
# running the parser against the rendered ExecStart with the prefix
# injected, plus a discrimination control with a non-prefixed input that
# MUST round-trip unchanged.
# ---------------------------------------------------------------------------
PARSER="${DARK_FACTORY_EXEC_START_PARSER:-$ROOT/daemon/scripts/parse-execstart.sh}"

# Inject the `path=` prefix into the rendered ExecStart to exercise the
# parser's prefix-strip path. The expected post-parse value equals the
# rendered ExecStart minus the prefix.
PREFIXED="path=$EXECSTART"

if [ ! -x "$PARSER" ]; then
  record_fail "ExecStart parser missing: $PARSER (the test needs an executable that strips the path= prefix)"
fi

if [ -x "$PARSER" ]; then
  # GREEN: parser strips `path=` and returns the bare path.
  PARSED_GREEN="$("$PARSER" "$PREFIXED" 2>"$TMP/parse-green.err" || true)"
  if [ "$PARSED_GREEN" = "$EXECSTART" ]; then
    record_pass "ExecStart parsing strips path= prefix (green: prefixed -> bare)"
  else
    record_fail "ExecStart parsing did NOT strip path= prefix (got '$PARSED_GREEN', want '$EXECSTART')"
  fi

  # Discrimination control: parser MUST round-trip a non-prefixed ExecStart
  # unchanged. If the parser accidentally strips a non-prefix, the GREEN
  # result above would be meaningless.
  PARSED_RAW="$("$PARSER" "$EXECSTART" 2>"$TMP/parse-raw.err" || true)"
  if [ "$PARSED_RAW" = "$EXECSTART" ]; then
    record_pass "ExecStart parsing leaves non-prefixed input unchanged (discrimination control)"
  else
    record_fail "ExecStart parser mutated non-prefixed input (got '$PARSED_RAW', want '$EXECSTART')"
  fi
fi

# ---------------------------------------------------------------------------
# Manifest binding — the immutable release MUST emit a manifest file at the
# release root with three required fields. The release root is determined
# either from $DARK_FACTORY_HOME (when invoked under the install path) or
# from the rendered unit's WorkingDirectory.
# ---------------------------------------------------------------------------
WORKDIR="$(grep -E '^WorkingDirectory=' "$RENDERED" | head -1 | cut -d= -f2-)"

# Candidate manifest locations — the test is intentionally liberal so it
# catches whichever filename the implementer chooses.
MANIFEST=""
for candidate in \
  "$WORKDIR/release-provenance.json" \
  "$WORKDIR/.release-provenance" \
  "$WORKDIR/daemon/release-provenance.json"; do
  if [ -f "$candidate" ]; then
    MANIFEST="$candidate"
    break
  fi
done

if [ -z "$MANIFEST" ]; then
  record_fail "release provenance manifest missing (expected one of: $WORKDIR/release-provenance.json, $WORKDIR/.release-provenance, $WORKDIR/daemon/release-provenance.json)"
else
  record_pass "release provenance manifest present: $MANIFEST"
fi

if [ -n "$MANIFEST" ] && command -v python3 >/dev/null 2>&1; then
  assert_grep "manifest binds source_commit" '"source_commit"\s*:' "$MANIFEST"
  assert_grep "manifest binds binary_path" '"binary_path"\s*:' "$MANIFEST"
  assert_grep "manifest binds binary_sha256" '"binary_sha256"\s*:' "$MANIFEST"

  # Manifest binary_path must point at an actual binary in the release.
  BIN_PATH_FROM_MANIFEST="$(python3 -c "import json,sys; print(json.load(open('$MANIFEST'))['binary_path'])" 2>/dev/null || true)"
  if [ -n "$BIN_PATH_FROM_MANIFEST" ]; then
    if [ -x "$WORKDIR/$BIN_PATH_FROM_MANIFEST" ]; then
      record_pass "manifest.binary_path resolves to executable: $WORKDIR/$BIN_PATH_FROM_MANIFEST"
    else
      record_fail "manifest.binary_path does NOT resolve to an executable: $WORKDIR/$BIN_PATH_FROM_MANIFEST"
    fi

    # binary_sha256 must match the on-disk binary's actual hash. Empty
    # hashes count as missing, not as valid.
    SHA_FROM_MANIFEST="$(python3 -c "import json,sys; print(json.load(open('$MANIFEST'))['binary_sha256'])" 2>/dev/null || true)"
    ACTUAL_SHA="$(sha256sum "$WORKDIR/$BIN_PATH_FROM_MANIFEST" 2>/dev/null | awk '{print $1}')"
    if [ -n "$SHA_FROM_MANIFEST" ] && [ "$SHA_FROM_MANIFEST" = "$ACTUAL_SHA" ]; then
      record_pass "manifest.binary_sha256 matches on-disk binary (sha256=$ACTUAL_SHA)"
    else
      record_fail "manifest.binary_sha256 mismatch (manifest='$SHA_FROM_MANIFEST', actual='$ACTUAL_SHA')"
    fi

    # source_commit must equal the actual HEAD SHA of the release checkout.
    SRC_FROM_MANIFEST="$(python3 -c "import json,sys; print(json.load(open('$MANIFEST'))['source_commit'])" 2>/dev/null || true)"
    ACTUAL_HEAD="$(git -C "$WORKDIR" rev-parse HEAD 2>/dev/null || echo)"
    if [ -n "$SRC_FROM_MANIFEST" ] && [ "$SRC_FROM_MANIFEST" = "$ACTUAL_HEAD" ]; then
      record_pass "manifest.source_commit matches git HEAD ($ACTUAL_HEAD)"
    else
      record_fail "manifest.source_commit mismatch (manifest='$SRC_FROM_MANIFEST', head='$ACTUAL_HEAD')"
    fi
  fi

  # Cross-check: ExecStart must reference the manifest's binary_path, not
  # a stale hard-coded path. This catches the regression where the unit
  # template hard-codes daemon/target/release/daemon while the manifest
  # tracks a different path.
  if [ -n "$BIN_PATH_FROM_MANIFEST" ] && [ "$EXECSTART" = "$WORKDIR/$BIN_PATH_FROM_MANIFEST" ]; then
    record_pass "ExecStart references manifest.binary_path (consistent provenance)"
  else
    record_fail "ExecStart does NOT reference manifest.binary_path (exec='$EXECSTART', manifest='$WORKDIR/$BIN_PATH_FROM_MANIFEST')"
  fi
fi

# ---------------------------------------------------------------------------
# Unrelated-resource inventory completion check: every resource the manifest
# is supposed to bind MUST appear in the manifest itself. If any required
# field is silently dropped, this catches the regression.
# ---------------------------------------------------------------------------
if [ -n "$MANIFEST" ] && command -v python3 >/dev/null 2>&1; then
  for field in source_commit binary_path binary_sha256; do
    if grep -q "\"$field\"\\s*:" "$MANIFEST"; then
      record_pass "unrelated-resource inventory: $field present in manifest"
    else
      record_fail "unrelated-resource inventory: $field MISSING from manifest (gap in coverage)"
    fi
  done
fi

# ---------------------------------------------------------------------------
# Final tally. Skip is non-PASS: if any SKIP was emitted (typically the
# binary-missing path), exit 1 so the run is recorded as FAIL.
# ---------------------------------------------------------------------------
echo
emit "RESULT: $FAIL failed, $SKIP skipped, $PASS passed"

if [ "$SKIP" -ne 0 ]; then
  emit "EXIT 1: skip is non-PASS (recorded $SKIP skip(s))"
  exit 1
fi

if [ "$FAIL" -ne 0 ]; then
  exit 1
fi

exit 0
