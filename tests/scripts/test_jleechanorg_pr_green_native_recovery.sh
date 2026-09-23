#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=../../jobs/jleechanorg-pr-green/session-reuse.sh
source "$ROOT/jobs/jleechanorg-pr-green/session-reuse.sh"

fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT
intended_home="$fixture_dir/codex-dark-factory"
source_home="$fixture_dir/codex-old"
workspace="$fixture_dir/worktree"
mock_bin="$fixture_dir/bin"
mkdir -p "$intended_home" "$source_home/sessions/2026/09/23" "$workspace" "$mock_bin"
printf '%s\n' '{"tokens":{}}' >"$intended_home/auth.json"
export CODEX_HOME="$intended_home"
export PR_GREEN_CODEX_HOME_CANDIDATES="$source_home:$intended_home"
export PR_GREEN_AO_DB_PATH="$fixture_dir/ao.db"
: >"$PR_GREEN_AO_DB_PATH"
db_state="$fixture_dir/db-state.tsv"
export PR_GREEN_TEST_DB_STATE="$db_state"
printf '%s||1\n' "$workspace" >"$db_state"
cat >"$mock_bin/sqlite3" <<'EOF'
#!/usr/bin/env bash
cat "$PR_GREEN_TEST_DB_STATE"
EOF
chmod +x "$mock_bin/sqlite3"
export PATH="$mock_bin:$PATH"

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
  printf '%s|%s|1\n' "$workspace" "$native_id" >"$PR_GREEN_TEST_DB_STATE"
  printf '%s\n' '{"status":"ok"}'
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

# Live sessions are not restored or killed, but an already-known native
# conversation can be copied into the intended profile before ordinary reuse.
printf '%s|%s|0\n' "$workspace" "$native_id" >"$db_state"
rm -f "$rollout_target"
pr_green_preserve_live_native_conversation worldarchitect.ai worldarchitect.ai-123
[[ -f "$rollout_target" ]] || {
  echo 'FAIL: live native rollout was not preserved for the intended profile' >&2
  exit 1
}
[[ "$(wc -l <"$curl_calls")" == "$curl_calls_before" ]] || {
  echo 'FAIL: live profile migration unexpectedly posted activity' >&2
  exit 1
}

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
printf '%s\n' '{"type":"session_meta","payload":{"id":"ambiguous-a","cwd":"'"$ambiguous_workspace"'"}}' >"$source_home/sessions/2026/09/23/rollout-a.jsonl"
printf '%s\n' '{"type":"session_meta","payload":{"session_id":"ambiguous-b","cwd":"'"$ambiguous_workspace"'"}}' >"$source_home/sessions/2026/09/23/rollout-b.jsonl"
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
