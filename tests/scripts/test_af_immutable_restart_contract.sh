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
AO_KILLED_FILE="$TMP/ao-killed"
AO_MALFORMED_AFTER_RESTART_FILE="$TMP/ao-malformed-after-restart"

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
  if [ "${FAKE_AO_MALFORMED_AFTER_RESTART:-0}" = "1" ]; then
    touch "$FAKE_AO_MALFORMED_AFTER_RESTART_FILE"
  fi
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
if [ "${1:-}" = "session" ] && [ "${2:-}" = "get" ] && [ "${3:-}" = "restart-contract-session" ] && [ "${4:-}" = "-p" ] && [ "${6:-}" = "--json" ]; then
  if [ "${FAKE_AO_SESSION_GET_MISSING:-0}" = "1" ]; then
    printf '{"project":"dark-factory-contract-owned","branch":"factory/restart-contract","workspace_path":"%s","worker_pid":"%s","runtime":"tmux"}\n' "$FAKE_WORKER_PANE_PATH" "$(cat "$FAKE_WORKER_PID_FILE")"
  elif [ "${FAKE_AO_SESSION_GET_UNPREFIXED:-0}" = "1" ]; then
    printf '{"project":"dark-factory-contract-owned","branch":"factory/restart-contract","workspace_path":"%s","worker_pid":"%s","runtime":"tmux","tmux_session":"restart-contract-session"}\n' "$FAKE_WORKER_PANE_PATH" "$(cat "$FAKE_WORKER_PID_FILE")"
  else
    printf '{"project":"dark-factory-contract-owned","branch":"factory/restart-contract","workspace_path":"%s","worker_pid":"%s","runtime":"tmux","tmux_session":"host-restart-contract-session"}\n' "$FAKE_WORKER_PANE_PATH" "$(cat "$FAKE_WORKER_PID_FILE")"
  fi
  exit 0
fi
if [ "${1:-}" = "status" ] && [ "${2:-}" = "-p" ] && [ "${4:-}" = "--json" ]; then
  if [ "$3" = "$DARK_FACTORY_RESTART_AO_PROJECT" ]; then
    if [ "${FAKE_AO_MALFORMED:-0}" = "1" ]; then
      printf '{malformed status json\n'
      exit 0
    fi
    if [ "${FAKE_AO_PREAMBLE:-0}" = "1" ]; then
      printf 'ao notifier: status follows\n'
    fi
    printf '[{"name":"unrelated-production-session","project":"dark-factory-contract","branch":"factory/unrelated","role":"worker","status":"done","activity":"exited","lastActivity":"volatile","review":{"state":"stale"}}]\n'
    exit 0
  fi
  if [ "$3" = "$AO_IMMUTABLE_RESTART_TEST_PROJECT" ]; then
    if [ "${FAKE_AO_FAIL_TEST_STATUS:-0}" = "1" ]; then
      printf 'scripted test-project status failure\n' >&2
      exit 17
    fi
    if [ -f "$FAKE_AO_MALFORMED_AFTER_RESTART_FILE" ]; then
      printf '{malformed post-restart status json\n'
      exit 0
    fi
    if [ -f "$FAKE_AO_KILLED_FILE" ] && [ "${FAKE_AO_KEEP_OWNED:-0}" != "1" ]; then
      printf '[{"name":"restart-contract-session","project":"dark-factory-contract-owned","branch":"factory/restart-contract","role":"worker","status":"killed","activity":"exited","lastActivity":"volatile","review":{"state":"stale"}},{"name":"unrelated-test-session","project":"dark-factory-contract-owned","branch":"factory/unrelated","role":"worker","status":"done","activity":"exited","lastActivity":"volatile","review":{"state":"stale"}}]\n'
      exit 0
    fi
    printf '[{"name":"restart-contract-session","project":"dark-factory-contract-owned","branch":"factory/restart-contract","role":"worker","status":"working","activity":"ready","lastActivity":"volatile","review":{"state":"stale"}},{"name":"unrelated-test-session","project":"dark-factory-contract-owned","branch":"factory/unrelated","role":"worker","status":"done","activity":"exited","lastActivity":"volatile","review":{"state":"stale"}}]\n'
    exit 0
  fi
fi
if [ "${1:-}" = "session" ] && [ "${2:-}" = "kill" ] && [ "${3:-}" = "restart-contract-session" ]; then
  if [ "${FAKE_AO_KEEP_OWNED:-0}" != "1" ]; then
    touch "$FAKE_AO_KILLED_FILE"
  fi
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
if [ "${1:-}" = "list-panes" ] && [ "${2:-}" = "-a" ] && [ "${3:-}" = "-F" ]; then
  if [ ! -f "$FAKE_AO_KILLED_FILE" ] || [ "${FAKE_AO_KEEP_OWNED:-0}" = "1" ]; then
    printf 'host-restart-contract-session\t%s\t%s\n' \
      "$(cat "$FAKE_WORKER_PID_FILE")" "$FAKE_WORKER_PANE_PATH"
  fi
  printf 'unrelated-contract-session\t999999\t%s\n' "$FAKE_UNRELATED_PANE_PATH"
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
export FAKE_AO_KILLED_FILE="$AO_KILLED_FILE"
export FAKE_AO_MALFORMED_AFTER_RESTART_FILE="$AO_MALFORMED_AFTER_RESTART_FILE"
export FAKE_WORKER_PANE_PATH="$WORKTREE"
export FAKE_UNRELATED_PANE_PATH="$TMP/unrelated-worktree"

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

set +e
same_project_output="$(DARK_FACTORY_RESTART_AO_PROJECT="${AO_IMMUTABLE_RESTART_TEST_PROJECT}" bash "$HARNESS" 2>&1)"
same_project_rc=$?
set -e
if [ "$same_project_rc" -ne 2 ]; then
  echo "FAIL: production and disposable AO projects must be distinct (rc=2), got rc=$same_project_rc" >&2
  echo "$same_project_output" >&2
  exit 1
fi
printf '%s\n' "$same_project_output" | python3 -c '
import json, sys
text = sys.stdin.read()
start = text.find("{")
payload = json.loads(text[start:])
assert payload["status"] == "skipped", payload
assert "distinct" in payload["reason"], payload
'

set +e
malformed_output="$(FAKE_AO_MALFORMED=1 bash "$HARNESS" 2>&1)"
malformed_rc=$?
set -e
if [ "$malformed_rc" -ne 2 ]; then
  echo "FAIL: malformed production status JSON must be an explicit SKIP (rc=2), got rc=$malformed_rc" >&2
  echo "$malformed_output" >&2
  exit 1
fi
printf '%s\n' "$malformed_output" | python3 -c '
import json, sys
text = sys.stdin.read()
start = text.find("{")
payload = json.loads(text[start:])
assert payload["status"] == "skipped", payload
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

output="$(FAKE_AO_PREAMBLE=1 bash "$HARNESS")"
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
assert worker["tmux_session_before"] == "host-restart-contract-session", worker
assert worker["tmux_session_after"] == "host-restart-contract-session", worker
assert worker["tmux_pane_current_path_before"] == os.environ.get("FAKE_WORKER_PANE_PATH"), worker
assert worker["tmux_pane_current_path_after"] == os.environ.get("FAKE_WORKER_PANE_PATH"), worker
assert worker["tmux_pane_branch_before"] == "factory/restart-contract", worker
assert worker["tmux_pane_branch_after"] == "factory/restart-contract", worker
assert target["unrelated_inventory_before"]["production"] == target["unrelated_inventory_after"]["production"], target
assert target["unrelated_inventory_before"]["disposable"] == target["unrelated_inventory_after"]["disposable"], target
assert target["unrelated_inventory_before"]["tmux"] == target["unrelated_inventory_after"]["tmux"], target
stable = {"name", "project", "branch", "role", "status", "activity"}
for inventory in (target["unrelated_inventory_before"]["production"], target["unrelated_inventory_before"]["disposable"]):
    for row in inventory:
        assert set(row) <= stable, row
        assert "lastActivity" not in row and "review" not in row, row
assert "worktree" not in worker, worker
PY

rm -f "$AO_KILLED_FILE"
worker_pid="$(python3 -c '
import subprocess
p = subprocess.Popen(["sleep", "120"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
print(p.pid)
')"
printf '%s\n' "$worker_pid" > "$WORKER_PID_FILE"
set +e
post_restart_malformed_output="$(FAKE_AO_MALFORMED_AFTER_RESTART=1 bash "$HARNESS" 2>&1)"
post_restart_malformed_rc=$?
set -e
if [ "$post_restart_malformed_rc" -ne 2 ]; then
  echo "FAIL: malformed post-restart status JSON must be an explicit SKIP (rc=2), got rc=$post_restart_malformed_rc" >&2
  echo "$post_restart_malformed_output" >&2
  exit 1
fi
printf '%s\n' "$post_restart_malformed_output" | python3 -c '
import json, sys
text = sys.stdin.read()
start = text.find("{")
payload = json.loads(text[start:])
assert payload["status"] == "skipped", payload
assert "after restart" in payload["reason"], payload
'
rm -f "$AO_MALFORMED_AFTER_RESTART_FILE"
set +e
missing_fields_output="$(FAKE_AO_SESSION_GET_MISSING=1 bash "$HARNESS" 2>&1)"
missing_fields_rc=$?
set -e
if [ "$missing_fields_rc" -ne 2 ]; then
  echo "FAIL: missing authoritative session fields must be an explicit SKIP, got rc=$missing_fields_rc" >&2
  echo "$missing_fields_output" >&2
  exit 1
fi
printf '%s\n' "$missing_fields_output" | python3 -c '
import json, sys
text = sys.stdin.read()
start = text.find("{")
payload = json.loads(text[start:])
assert payload["status"] == "skipped", payload
assert "authoritative" in payload["reason"], payload
'
set +e
unprefixed_output="$(FAKE_AO_SESSION_GET_UNPREFIXED=1 bash "$HARNESS" 2>&1)"
unprefixed_rc=$?
set -e
if [ "$unprefixed_rc" -ne 2 ]; then
  echo "FAIL: unprefixed inspection name must not suffix-match a host-prefixed pane, got rc=$unprefixed_rc" >&2
  echo "$unprefixed_output" >&2
  exit 1
fi
printf '%s\n' "$unprefixed_output" | python3 -c '
import json, sys
text = sys.stdin.read()
start = text.find("{")
payload = json.loads(text[start:])
assert payload["status"] == "skipped", payload
assert "correlate" in payload["reason"], payload
'
set +e
owned_persists_output="$(FAKE_AO_KEEP_OWNED=1 bash "$HARNESS" 2>&1)"
owned_persists_rc=$?
set -e
if [ "$owned_persists_rc" -ne 1 ]; then
  echo "FAIL: active owned AO row/replacement tmux pane must fail cleanup proof, got rc=$owned_persists_rc" >&2
  echo "$owned_persists_output" >&2
  exit 1
fi
if [[ "$owned_persists_output" != *"remains active"* ]]; then
  echo "FAIL: cleanup failure must identify the active owned AO row" >&2
  echo "$owned_persists_output" >&2
  exit 1
fi

echo "PASS: immutable restart manifest and identity contract"
