#!/usr/bin/env bash
# build_sqlite3_amalgamation.sh — portable sqlite3 CLI build for self-hosted runners.
#
# The dark-factory org's Linux container runners (ez-runner-c-7 et al.) DO NOT
# ship the sqlite3 binary. factory-overlay.sh:77 defines
#   sql() { sqlite3 -cmd '.timeout 5000' "$DB" "$@"; }
# and every overlay subcommand shells out to that binary, so the bash
# integration tests (tests/scripts/*.sh) fail with
#   "daemon/factory-overlay.sh: line 77: sqlite3: command not found"
# and report empty state (expected 'DISPATCHED', got '').
#
# apt-get install sqlite3 is the obvious fix BUT fails on these runners
# because they are started with the no-new-privileges flag, which blocks
# sudo. A python-stdlib shim was attempted and reverted: the overlay
# depends on sqlite3 CLI behavior (PRAGMA, -separator, -json) that the
# stdlib module does not reproduce.
#
# The proven alternative: download the official sqlite.org amalgamation
# (~3MB sqlite3.c + shell.c), compile with gcc into <bin_dir>/sqlite3,
# and export that bin dir ahead of /usr/bin on PATH. Works on every
# Linux architecture the org runners ship (x86_64 + aarch64). Pattern
# lifted from PR #288 / commits cc4fa1a4 + 0876784e which solved the
# same problem for the daemon-tests job's cargo test suite.
#
# Usage: build_sqlite3_amalgamation.sh <bin_dir>
#
# On success, <bin_dir>/sqlite3 exists and runs `:memory: 'SELECT 1;'`.
# On any failure, exits non-zero with an actionable error to stderr.
set -euo pipefail

BIN_DIR="${1:-}"
if [ -z "$BIN_DIR" ]; then
  echo "usage: $0 <bin_dir>" >&2
  echo "  Builds the sqlite3 CLI from the official amalgamation into <bin_dir>." >&2
  exit 2
fi

mkdir -p "$BIN_DIR"

# Idempotency: if a working sqlite3 already exists at BIN_DIR/sqlite3, skip
# the build. This makes the script safe to invoke from multiple CI steps
# (test job + daemon-tests job) without re-downloading ~3MB every run.
if [ -x "$BIN_DIR/sqlite3" ] && "$BIN_DIR/sqlite3" ':memory:' 'SELECT 1;' >/dev/null 2>&1; then
  echo "build_sqlite3_amalgamation: $BIN_DIR/sqlite3 already present ($("$BIN_DIR/sqlite3" --version))"
  exit 0
fi

# Pin the amalgamation version. The 2026 archive exists on sqlite.org as of
# 2026-07 (sqlite-tools-linux-x64 shipped from the same URL tree). If the
# upstream URL moves, the YEAR/VER pair must be repointed together — see
# commit 82cbd618 / 3e13cf7a for the prior re-pointing incident.
SQLITE_VER="3530300"
SQLITE_YEAR="2026"
AMALG_URL="https://sqlite.org/${SQLITE_YEAR}/sqlite-amalgamation-${SQLITE_VER}.zip"

# Toolchain guards. Self-hosted runners don't ship gcc by default on every
# image; surface a missing toolchain as a hard error instead of letting the
# script drag through to a confusing "no such file" at link time.
if ! command -v gcc >/dev/null 2>&1; then
  echo "build_sqlite3_amalgamation: gcc not found on PATH; cannot build sqlite3 from amalgamation" >&2
  exit 9
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "build_sqlite3_amalgamation: curl not found on PATH; cannot download amalgamation" >&2
  exit 9
fi
if ! command -v unzip >/dev/null 2>&1; then
  echo "build_sqlite3_amalgamation: unzip not found on PATH; cannot unpack amalgamation" >&2
  exit 9
fi

TMP_DIR="$(mktemp -d -t sqlite3-amalg.XXXXXX)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "build_sqlite3_amalgamation: downloading ${AMALG_URL}"
if ! curl -fsSL -o "$TMP_DIR/amalg.zip" "$AMALG_URL"; then
  echo "build_sqlite3_amalgamation: download failed (curl rc=$?); check that ${SQLITE_YEAR}/${SQLITE_VER} still exists on sqlite.org" >&2
  exit 9
fi

# sqlite.org zip is ~3MB, but unzip silently produces nothing on a corrupt
# archive. Verify size BEFORE unpacking so we fail fast.
AMALG_BYTES="$(stat -c %s "$TMP_DIR/amalg.zip" 2>/dev/null || stat -f %z "$TMP_DIR/amalg.zip")"
if [ -z "$AMALG_BYTES" ] || [ "$AMALG_BYTES" -lt 1000000 ]; then
  echo "build_sqlite3_amalgamation: download looks too small (${AMALG_BYTES} bytes); refusing to build" >&2
  exit 9
fi

unzip -qo "$TMP_DIR/amalg.zip" -d "$TMP_DIR/amalg"
SRC_DIR="$(find "$TMP_DIR/amalg" -maxdepth 2 -type d -name 'sqlite-amalgamation-*' | head -1)"
if [ -z "$SRC_DIR" ] || [ ! -f "$SRC_DIR/shell.c" ] || [ ! -f "$SRC_DIR/sqlite3.c" ]; then
  echo "build_sqlite3_amalgamation: could not locate shell.c / sqlite3.c under $TMP_DIR/amalg" >&2
  exit 9
fi

# Compile. -O2 matches the official amalgamation's default release profile;
# -lpthread / -ldl / -lm cover POSIX threading, dynamic loader, and math.
# Do NOT strip the binary — `sqlite3 :memory: 'SELECT 1;'` below must be able
# to dlopen libsqlite3 symbols, and stripping can mask a missing linker flag
# that would otherwise surface as a clear runtime error.
echo "build_sqlite3_amalgamation: compiling $BIN_DIR/sqlite3 from amalgamation"
if ! gcc -O2 -o "$BIN_DIR/sqlite3" \
      "$SRC_DIR/shell.c" \
      "$SRC_DIR/sqlite3.c" \
      -lpthread -ldl -lm; then
  echo "build_sqlite3_amalgamation: gcc compile failed (rc=$?)" >&2
  exit 9
fi

# Sanity smoke: same query the daemon-tests job uses. A silent "build
# succeeded" with a missing -lpthread is the worst class of bug — it shows
# up as a crash on the FIRST concurrent PRAGMA in the overlay suite.
if ! "$BIN_DIR/sqlite3" ':memory:' 'SELECT 1;' >/dev/null 2>&1; then
  echo "build_sqlite3_amalgamation: built sqlite3 failed SELECT 1 smoke" >&2
  exit 9
fi

echo "build_sqlite3_amalgamation: built $("$BIN_DIR/sqlite3" --version) into $BIN_DIR"