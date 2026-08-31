#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HARNESS="$ROOT/tests/scripts/test_af_immutable_restart.sh"
TMP="$(mktemp -d -t df-restart-contract.XXXXXX)"
FAKE_BIN="$TMP/bin"
RELEASE_COMMIT="0123456789abcdef0123456789abcdef01234567"
RELEASE="$TMP/releases/$RELEASE_COMMIT"
DAEMON_BINARY="$RELEASE/daemon/target/release/daemon"
MANIFEST="$RELEASE/release-manifest.json"
WORKTREE="$TMP/target-worktree"
DAEMON_PID_FILE="$TMP/daemon.pid"
DAEMON_PATHS_FILE="$TMP/daemon-paths"
WORKER_PID_FILE="$TMP/worker.pid"

cleanup() {
  local rc=$?
  if [ -s "$DAEMON_PID_FILE" ]; then
    kill "$(cat "$DAEMON_PID_FILE")" >/dev/null 2>&1 || true
  fi
  if [ -s "$WORKER_PID_FILE" ]; then
    kill "$(cat "$WORKER_PID_FILE")" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP"
  exit "$rc"
}
trap cleanup EXIT INT TERM

mkdir -p "$FAKE_BIN" "$(dirname "$DAEMON_BINARY")" "$WORKTREE"
cp "$(command -v sleep)" "$DAEMON_BINARY"
chmod +x "$DAEMON_BINARY"

git -C "$WORKTREE" init -q
git -C "$WORKTREE" checkout -q -b factory/restart-contract
git -C "$WORKTREE" -c user.name=test -c user.email=test@example.invalid \
  commit -q --allow-empty -m initial

DAEMON_SHA256="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$DAEMON_BINARY")"
python3 - "$MANIFEST" "$RELEASE_COMMIT" "$DAEMON_SHA256" <<'PY'
import json
import sys

path, commit, digest = sys.argv[1:]
with open(path, "w", encoding="utf-8") as handle:
    json.dump({
        "schema_version": 1,
        "source_commit": commit,
        "daemon": {
            "path": "daemon/target/release/daemon",
            "sha256": digest,
        },
    }, handle)
PY

cat > "$FAKE_BIN/systemctl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
args=("$@")
if [ "${args[*]}" = "--user show-environment" ]; then
  exit 0
fi
if [ "${args[*]}" = "--user is-active --quiet $DARK_FACTORY_RESTART_UNIT" ]; then
  exit 0
fi
if [ "${args[*]}" = "--user show $DARK_FACTORY_RESTART_UNIT --property=MainPID --value" ]; then
  cat "$FAKE_DAEMON_PID_FILE"
  exit 0
fi
if [ "${args[*]}" = "--user show $DARK_FACTORY_RESTART_UNIT --property=ExecStart --value" ]; then
  printf '{ path=%s ; argv[]=%s ; ignore_errors=no ; }\n' "$FAKE_DAEMON_BINARY" "$FAKE_DAEMON_BINARY"
  exit 0
fi
if [ "${args[*]}" = "--user restart $DARK_FACTORY_RESTART_UNIT" ]; then
  old_pid="$(cat "$FAKE_DAEMON_PID_FILE")"
  kill "$old_pid" >/dev/null 2>&1 || true
  "$FAKE_DAEMON_BINARY" 120 &
  new_pid=$!
  printf '%s\n' "$new_pid" > "$FAKE_DAEMON_PID_FILE"
  printf '%s %s\n' "$new_pid" "$FAKE_DAEMON_BINARY" >> "$FAKE_DAEMON_PATHS_FILE"
  exit 0
fi
printf 'unexpected systemctl argv: %q\n' "${args[*]}" >&2
exit 97
SH
chmod +x "$FAKE_BIN/systemctl"

cat > "$FAKE_BIN/readlink" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = "-f" ] || exit 96
target="${2:-}"
case "$target" in
  /proc/*/exe)
    pid="${target#/proc/}"
    pid="${pid%/exe}"
    found="$(awk -v wanted="$pid" '$1 == wanted { found=$2 } END { if (found != "") print found }' "$FAKE_DAEMON_PATHS_FILE")"
    [ -n "$found" ]
    python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$found"
    ;;
  *)
    python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$target"
    ;;
esac
SH
chmod +x "$FAKE_BIN/readlink"

cat > "$FAKE_BIN/sha256sum" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
python3 -c 'import hashlib,sys; p=sys.argv[1]; print(hashlib.sha256(open(p, "rb").read()).hexdigest(), p)' "$1"
SH
chmod +x "$FAKE_BIN/sha256sum"

cat > "$FAKE_BIN/ao" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "status" ] && [ "${2:-}" = "-p" ] && [ "${4:-}" = "--json" ]; then
  if [ "$3" = "$DARK_FACTORY_RESTART_AO_PROJECT" ]; then
    printf '[]\n'
    exit 0
  fi
  if [ "$3" = "$AO_IMMUTABLE_RESTART_TEST_PROJECT" ]; then
    if [ "${FAKE_AO_FAIL_TEST_STATUS:-0}" = "1" ]; then
      printf 'scripted test-project status failure\n' >&2
      exit 17
    fi
    printf '[{"name":"restart-contract-session","branch":"factory/restart-contract","status":"working","activity":"ready"}]\n'
    exit 0
  fi
fi
if [ "${1:-}" = "session" ] && [ "${2:-}" = "kill" ] && [ "${3:-}" = "restart-contract-session" ]; then
  kill "$(cat "$FAKE_WORKER_PID_FILE")"
  exit 0
fi
printf 'unexpected ao argv: %q\n' "$*" >&2
exit 98
SH
chmod +x "$FAKE_BIN/ao"

cat > "$FAKE_BIN/tmux" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "list-panes" ] && [ "${2:-}" = "-t" ] && [ "${3:-}" = "restart-contract-session" ]; then
  cat "$FAKE_WORKER_PID_FILE"
  exit 0
fi
exit 99
SH
chmod +x "$FAKE_BIN/tmux"

daemon_pid="$(python3 -c '
import subprocess
import sys
p = subprocess.Popen([sys.argv[1], "120"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
print(p.pid)
' "$DAEMON_BINARY")"
printf '%s\n' "$daemon_pid" > "$DAEMON_PID_FILE"
printf '%s %s\n' "$daemon_pid" "$DAEMON_BINARY" > "$DAEMON_PATHS_FILE"
worker_pid="$(python3 -c '
import subprocess
p = subprocess.Popen(["sleep", "120"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
print(p.pid)
')"
printf '%s\n' "$worker_pid" > "$WORKER_PID_FILE"

export PATH="$FAKE_BIN:$PATH"
export DARK_FACTORY_RESTART_UNIT="ai.dark-factory.contract-test.service"
export DARK_FACTORY_RESTART_AO_PROJECT="dark-factory-contract"
export DARK_FACTORY_RESTART_WORKTREE="$WORKTREE"
export DARK_FACTORY_RESTART_SETTLE_SECS=0
export AO_IMMUTABLE_RESTART_TEST_PROJECT="dark-factory-contract-owned"
export FAKE_DAEMON_BINARY="$DAEMON_BINARY"
export FAKE_DAEMON_PID_FILE="$DAEMON_PID_FILE"
export FAKE_DAEMON_PATHS_FILE="$DAEMON_PATHS_FILE"
export FAKE_WORKER_PID_FILE="$WORKER_PID_FILE"

set +e
skip_output="$(FAKE_AO_FAIL_TEST_STATUS=1 bash "$HARNESS" 2>&1)"
skip_rc=$?
set -e
if [ "$skip_rc" -ne 2 ]; then
  echo "FAIL: test-project status failure must be an explicit SKIP (rc=2), got rc=$skip_rc" >&2
  echo "$skip_output" >&2
  exit 1
fi
printf '%s\n' "$skip_output" | python3 -c '
import json, sys
text = sys.stdin.read()
start = text.find("{")
payload = json.loads(text[start:])
assert payload["status"] == "skipped", payload
assert "status" in payload["reason"] and "rc=17" in payload["reason"], payload
'

cp "$MANIFEST" "$MANIFEST.good"
python3 - "$MANIFEST" <<'PY'
import json
import sys

path = sys.argv[1]
data = json.load(open(path, encoding="utf-8"))
data["daemon"]["sha256"] = "0" * 64
json.dump(data, open(path, "w", encoding="utf-8"))
PY
set +e
bad_manifest_output="$(bash "$HARNESS" 2>&1)"
bad_manifest_rc=$?
set -e
if [ "$bad_manifest_rc" -ne 1 ]; then
  echo "FAIL: manifest hash mismatch must fail, got rc=$bad_manifest_rc" >&2
  echo "$bad_manifest_output" >&2
  exit 1
fi
if [[ "$bad_manifest_output" != *"manifest"* ]]; then
  echo "FAIL: manifest mismatch failure must name the manifest" >&2
  echo "$bad_manifest_output" >&2
  exit 1
fi
mv "$MANIFEST.good" "$MANIFEST"

output="$(bash "$HARNESS")"
REPORT_JSON="$output" python3 - "$RELEASE_COMMIT" "$DAEMON_SHA256" "$DAEMON_BINARY" <<'PY'
import json
import os
import sys

commit, digest, binary = sys.argv[1:]
binary = os.path.realpath(binary)
report = json.loads(os.environ["REPORT_JSON"])
target = report["restart_target"]
worker = report["worker_continuity_proof"]
assert report["status"] == "passed", report
assert target["release_commit"] == commit, target
assert target["binary_sha256_before"] == digest, target
assert target["binary_sha256_after"] == digest, target
assert target["proc_exe_before"] == binary, target
assert target["proc_exe_after"] == binary, target
assert target["manifest_cross_check"]["status"] == "verified", target
assert target["manifest_cross_check"]["path"].endswith("/release-manifest.json"), target
assert worker["ao_session"] == "restart-contract-session", worker
assert worker["session_branch_before"] == "factory/restart-contract", worker
assert worker["session_branch_after"] == "factory/restart-contract", worker
assert "worktree" not in worker, worker
PY

echo "PASS: immutable restart manifest and identity contract"
