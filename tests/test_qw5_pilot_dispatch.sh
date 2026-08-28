#!/usr/bin/env bash
set -euo pipefail

# End-to-end lifecycle regression for daemon/qw5-pilot-dispatch.sh.
# The script is copied only to redirect its macOS-specific absolute paths into
# a disposable fixture; all dispatch and MiniMax lifecycle logic is real.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/qw5-dispatch-test.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

FIXTURE_REPO="$TMP_DIR/repo"
ORIGIN_REPO="$TMP_DIR/origin.git"
WORKTREE="$TMP_DIR/worktree"
BIN_DIR="$TMP_DIR/bin"
HOME_DIR="$TMP_DIR/home"
LOG_DIR="$TMP_DIR/logs"
SENTINEL="$TMP_DIR/dispatch.done"
DISPATCH="$TMP_DIR/qw5-pilot-dispatch.sh"
CALLS="$TMP_DIR/claudem.calls"
LAUNCHCTL_CALLS="$TMP_DIR/launchctl.calls"

mkdir -p "$BIN_DIR" "$HOME_DIR" "$LOG_DIR" "$FIXTURE_REPO/daemon"

git init -q --initial-branch=main "$FIXTURE_REPO"
git -C "$FIXTURE_REPO" config user.email 'qw5-test@example.invalid'
git -C "$FIXTURE_REPO" config user.name 'QW5 Dispatch Test'
printf 'test prompt\n' > "$FIXTURE_REPO/daemon/qw5-coder-prompt.md"
git -C "$FIXTURE_REPO" add daemon/qw5-coder-prompt.md
git -C "$FIXTURE_REPO" commit -qm 'fixture'

git init -q --bare "$ORIGIN_REPO"
git -C "$FIXTURE_REPO" remote add origin "$ORIGIN_REPO"
git -C "$FIXTURE_REPO" push -q origin main

cat > "$BIN_DIR/claudem" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$CALLS"
exit 0
SHIM
chmod +x "$BIN_DIR/claudem"

cat > "$BIN_DIR/launchctl" <<'SHIM'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$LAUNCHCTL_CALLS"
exit 0
SHIM
chmod +x "$BIN_DIR/launchctl"

cp "$ROOT/daemon/qw5-pilot-dispatch.sh" "$DISPATCH"
sed -i "s|^REPO=.*|REPO=\"$FIXTURE_REPO\"|" "$DISPATCH"
sed -i "s|^WT=.*|WT=\"$WORKTREE\"|" "$DISPATCH"
sed -i "s|^SENTINEL=.*|SENTINEL=\"$SENTINEL\"|" "$DISPATCH"
sed -i "s|^PLIST_SRC=.*|PLIST_SRC=\"$TMP_DIR/pilot.plist.template\"|" "$DISPATCH"
sed -i "s|^PLIST_DST=.*|PLIST_DST=\"$TMP_DIR/pilot.plist\"|" "$DISPATCH"
sed -i "s|^LOGDIR=.*|LOGDIR=\"$LOG_DIR\"|" "$DISPATCH"
chmod +x "$DISPATCH"

run_dispatch() {
  HOME="$HOME_DIR" \
    PATH="$BIN_DIR:/usr/bin:/bin" \
    CALLS="$CALLS" \
    LAUNCHCTL_CALLS="$LAUNCHCTL_CALLS" \
    "$@" bash "$DISPATCH"
}

set +e
run_dispatch env -u MINIMAX_API_KEY
first_rc=$?
set -e

if [[ "$first_rc" -ne 66 ]]; then
  echo "expected missing MiniMax key to exit 66, got $first_rc" >&2
  exit 1
fi
if [[ -e "$SENTINEL" ]]; then
  echo "missing MiniMax key consumed the one-shot sentinel" >&2
  exit 1
fi
if ! grep -q 'MINIMAX_API_KEY must be nonblank' "$LOG_DIR/jleechan-qw5-dispatch.stderr.log"; then
  echo "missing MiniMax key failure was not recorded" >&2
  exit 1
fi

run_dispatch env MINIMAX_API_KEY='test-minimax-key'

if [[ ! -f "$SENTINEL" ]]; then
  echo "successful retry did not create the one-shot sentinel" >&2
  exit 1
fi
if [[ "$(wc -l < "$CALLS")" -ne 1 ]]; then
  echo "successful retry did not invoke exactly one coder dispatch" >&2
  exit 1
fi
if ! grep -q -- '--bare' "$CALLS" || ! grep -q -- '--model MiniMax-M3' "$CALLS"; then
  echo "retry did not use the pinned MiniMax dispatch flags" >&2
  exit 1
fi

echo 'PASS: blank MiniMax key leaves sentinel untouched and a later retry dispatches'
