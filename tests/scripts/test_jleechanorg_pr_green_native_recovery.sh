#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../../jobs/jleechanorg-pr-green/session-reuse.sh
source "$ROOT/jobs/jleechanorg-pr-green/session-reuse.sh"

fixture_dir="$(mktemp -d)"
live_pid=''
trap '[[ -z "$live_pid" ]] || kill "$live_pid" 2>/dev/null || true; rm -rf "$fixture_dir"' EXIT
intended_home="$fixture_dir/codex-dark-factory"
source_home="$fixture_dir/codex-old"
workspace="$fixture_dir/worktree"
mock_bin="$fixture_dir/bin"
mkdir -p "$intended_home" "$source_home/sessions/2026/09/23" "$workspace" "$mock_bin"
printf '%s\n' '{"tokens":{}}' >"$intended_home/auth.json"
export CODEX_HOME="$intended_home"
export PR_GREEN_CODEX_HOME_CANDIDATES="$source_home:$intended_home"
export PR_GREEN_AO_DB_PATH="$fixture_dir/ao.db"
export PR_GREEN_AO_RUN_FILE="$fixture_dir/running.json"
: >"$PR_GREEN_AO_DB_PATH"
printf '%s\n' '{"pid":1234,"port":43123}' >"$PR_GREEN_AO_RUN_FILE"
db_state="$fixture_dir/db-state.tsv"
export PR_GREEN_TEST_DB_STATE="$db_state"
printf '%s||1\n' "$workspace" >"$db_state"
cat >"$mock_bin/sqlite3" <<'EOF'
#!/usr/bin/env bash
if [[ "$*" == *runtime_handle_id* ]]; then
  printf '%s\n' "${PR_GREEN_TEST_RUNTIME_HANDLE:?}"
else
  cat "$PR_GREEN_TEST_DB_STATE"
fi
EOF
chmod +x "$mock_bin/sqlite3"
export PATH="$mock_bin:$PATH"
export PR_GREEN_TEST_RUNTIME_HANDLE='worldarchitect-ai-123-runtime'
env CODEX_HOME="$intended_home" bash -c 'cd "$1" && exec sleep 60' bash "$workspace" &
live_pid=$!

native_id='native-session-123'
rollout_relative='sessions/2026/09/23/rollout-2026-09-23T08-00-00-native-session-123.jsonl'
rollout_source="$source_home/$rollout_relative"
rollout_target="$intended_home/$rollout_relative"
printf '%s\n' \
  "{\"type\":\"session_meta\",\"payload\":{\"session_id\":\"$native_id\",\"cwd\":\"$workspace\"}}" \
  '{"type":"event_msg","payload":{"type":"task_started"}}' >"$rollout_source"

db_mode='recoverable'
registered=0
ao_calls="$fixture_dir/ao-calls.log"
curl_calls="$fixture_dir/curl-calls.log"

ao() {
  printf '%s\n' "$*" >>"$ao_calls"
  case "$1 $2" in
    "status --json") printf '%s\n' '{"port":43123}' ;;
    "session ls") printf '%s\n' '{"data":[{"id":"worldarchitect.ai-123","displayName":"pr-123","isTerminated":true,"updatedAt":"2026-09-23T00:00:00Z"}]}' ;;
    "session restore"|"send --session") return 0 ;;
    *) printf 'unexpected ao call: %s\n' "$*" >&2; return 1 ;;
  esac
}

curl() {
  local body='' url=''
  while (($#)); do
    case "$1" in
      --data|--data-binary) body="$2"; shift 2 ;;
      http://*|https://*) url="$1"; shift ;;
      *) shift ;;
    esac
  done
  printf '%s\t%s\n' "$url" "$body" >>"$curl_calls"
  registered=1
  printf '%s|%s|%s\n' "$workspace" "$native_id" "${PR_GREEN_TEST_TERMINATED:-1}" >"$PR_GREEN_TEST_DB_STATE"
  printf '%s\n' '{"status":"ok"}'
}

tmux() {
  case "$1 $2" in
    "list-panes -t") printf '%s\n' "$live_pid" ;;
    *) printf 'unexpected tmux call: %s\n' "$*" >&2; return 1 ;;
  esac
}

pr_green_is_codex_process() {
  [[ "$1" == "$live_pid" ]]
}

pr_green_recover_native_conversation worldarchitect.ai worldarchitect.ai-123
[[ -f "$rollout_target" ]] || {
  echo 'FAIL: exact native rollout was not copied into intended CODEX_HOME' >&2
  exit 1
}
cmp -s "$rollout_source" "$rollout_target" || {
  echo 'FAIL: copied native rollout differs from source' >&2
  exit 1
}
grep -Fq 'http://127.0.0.1:43123/api/v1/sessions/worldarchitect.ai-123/activity' "$curl_calls" || {
  echo 'FAIL: recovery did not use the daemon port discovered from ao status' >&2
  exit 1
}
grep -Fq '{"agentSessionId":"native-session-123"}' "$curl_calls" || {
  echo 'FAIL: recovery did not register the exact native session id' >&2
  exit 1
}

# Existing native identity is idempotent and must not post again.
rm -f "$rollout_target"
curl_calls_before="$(wc -l <"$curl_calls")"
pr_green_recover_native_conversation worldarchitect.ai worldarchitect.ai-123
[[ -f "$rollout_target" ]] || {
  echo 'FAIL: registered native identity did not migrate its old-profile rollout' >&2
  exit 1
}
cmp -s "$rollout_source" "$rollout_target" || {
  echo 'FAIL: migrated registered rollout differs from source' >&2
  exit 1
}
[[ "$(wc -l <"$curl_calls")" == "$curl_calls_before" ]] || {
  echo 'FAIL: existing native identity was posted a second time' >&2
  exit 1
}

# A terminated rollout may have been copied before its final events arrived.
# An exact prefix is safe to extend, while divergent content must remain
# untouched and fail closed.
printf '%s\n' '{"type":"event_msg","payload":{"type":"task_finished"}}' >>"$rollout_source"
pr_green_recover_native_conversation worldarchitect.ai worldarchitect.ai-123
cmp -s "$rollout_source" "$rollout_target" || {
  echo 'FAIL: terminated native rollout prefix was not extended safely' >&2
  exit 1
}
# If the intended profile has a legitimate continuation while the old profile
# remains shorter, the longest mutually-prefix source must be preserved.
cp -p "$rollout_source" "$rollout_target"
printf '%s\n' '{"type":"event_msg","payload":{"type":"intended_profile_continuation"}}' >>"$rollout_target"
pr_green_recover_native_conversation worldarchitect.ai worldarchitect.ai-123
grep -Fq 'intended_profile_continuation' "$rollout_target" || {
  echo 'FAIL: longer intended-profile rollout was discarded for old shorter copy' >&2
  exit 1
}
printf '%s\n' 'divergent transcript' >"$rollout_target"
set +e
pr_green_recover_native_conversation worldarchitect.ai worldarchitect.ai-123
rc=$?
set -e
[[ "$rc" -ne 0 ]] || {
  echo 'FAIL: divergent terminated rollout was accepted' >&2
  exit 1
}
grep -Fqx 'divergent transcript' "$rollout_target" || {
  echo 'FAIL: divergent rollout target was overwritten' >&2
  exit 1
}
cp -p "$rollout_source" "$rollout_target"

# Live sessions are not restored, killed, or copied: the source rollout may
# still be appended by the running Codex process.
printf '%s|%s|0\n' "$workspace" "$native_id" >"$db_state"
rm -f "$rollout_target"
pr_green_preserve_live_native_conversation worldarchitect.ai worldarchitect.ai-123
[[ ! -e "$rollout_target" ]] || {
  echo 'FAIL: live native rollout was copied while its process could append' >&2
  exit 1
}
[[ "$(wc -l <"$curl_calls")" == "$curl_calls_before" ]] || {
  echo 'FAIL: live registered identity unexpectedly posted activity' >&2
  exit 1
}

# A live intended-profile process with an empty AO native id can be registered
# from its exact-cwd rollout without copying or mutating that live transcript.
cp -p "$rollout_source" "$rollout_target"
printf '%s||0\n' "$workspace" >"$db_state"
export PR_GREEN_TEST_TERMINATED=0
curl_calls_before="$(wc -l <"$curl_calls")"
pr_green_preserve_live_native_conversation worldarchitect.ai worldarchitect.ai-123
[[ "$(wc -l <"$curl_calls")" == "$((curl_calls_before + 1))" ]] || {
  echo 'FAIL: intended-profile live native identity was not registered' >&2
  exit 1
}
grep -Fq '{"agentSessionId":"native-session-123"}' "$curl_calls" || {
  echo 'FAIL: live registration used the wrong native identity' >&2
  exit 1
}
export PR_GREEN_TEST_TERMINATED=1
curl_calls_before="$(wc -l <"$curl_calls")"

# Multiple historical conversations are safe when AO creation time and the
# session_meta timestamp prove one newest identity; the helper must retain all
# rollout segments for that selected identity.
latest_workspace="$fixture_dir/latest-worktree"
mkdir -p "$latest_workspace"
printf '%s\n' '{"type":"session_meta","timestamp":"2026-09-23T08:00:00Z","payload":{"session_id":"native-old-123456","cwd":"'"$latest_workspace"'"}}' >"$source_home/sessions/2026/09/23/rollout-old.jsonl"
printf '%s\n' '{"type":"session_meta","timestamp":"2026-09-23T08:05:00Z","payload":{"session_id":"native-new-123456","cwd":"'"$latest_workspace"'"}}' >"$source_home/sessions/2026/09/23/rollout-new.jsonl"
latest_rollouts="$(pr_green_find_native_rollouts "$latest_workspace" '2026-09-23T08:04:00Z')"
latest_id="${latest_rollouts%%$'\n'*}"
[[ "$latest_id" == native-new-123456 ]] || {
  echo "FAIL: timestamp-qualified recovery selected $latest_id instead of newest native identity" >&2
  exit 1
}

# Multiple distinct session_meta identities for one exact cwd are ambiguous;
# no rollout copy or daemon mutation is allowed.
ambiguous_workspace="$fixture_dir/ambiguous-worktree"
mkdir -p "$ambiguous_workspace"
printf '%s\n' '{"type":"session_meta","timestamp":"2026-09-23T08:10:00Z","payload":{"id":"ambiguous-aaaaaaaaaaaa","cwd":"'"$ambiguous_workspace"'"}}' >"$source_home/sessions/2026/09/23/rollout-a.jsonl"
printf '%s\n' '{"type":"session_meta","timestamp":"2026-09-23T08:10:00Z","payload":{"session_id":"ambiguous-bbbbbbbbbbbb","cwd":"'"$ambiguous_workspace"'"}}' >"$source_home/sessions/2026/09/23/rollout-b.jsonl"
printf '%s||1\n' "$ambiguous_workspace" >"$PR_GREEN_TEST_DB_STATE"
set +e
pr_green_recover_native_conversation worldarchitect.ai worldarchitect.ai-ambiguous
rc=$?
set -e
[[ "$rc" -ne 0 ]] || {
  echo 'FAIL: ambiguous native session discovery was accepted' >&2
  exit 1
}
[[ "$(wc -l <"$curl_calls")" == "$curl_calls_before" ]] || {
  echo 'FAIL: ambiguous recovery contacted the daemon' >&2
  exit 1
}

# Live sessions are left untouched; recovery is only for terminated sessions
# immediately before the supported AO restore path.
printf '%s||0\n' "$workspace" >"$PR_GREEN_TEST_DB_STATE"
curl_calls_before="$(wc -l <"$curl_calls")"
pr_green_recover_native_conversation worldarchitect.ai worldarchitect.ai-live
[[ "$(wc -l <"$curl_calls")" == "$curl_calls_before" ]] || {
  echo 'FAIL: live session recovery changed native identity' >&2
  exit 1
}

echo 'jleechanorg-pr-green native conversation recovery: PASS'
