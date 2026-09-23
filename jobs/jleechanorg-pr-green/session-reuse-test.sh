#!/usr/bin/env bash
set -euo pipefail

job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=session-reuse.sh
source "$job_dir/session-reuse.sh"

fixture='{"data":[{"id":"wa-live","displayName":"pr-123","isTerminated":false,"status":"pr_open","updatedAt":"2026-09-23T00:00:00Z"}]}'
session_get_fixture=''
busy_runtime_handle=''
busy_pane_output=''
send_fail=0
restore_fail=0
recovery_dir="$(mktemp -d)"
recovery_home="$recovery_dir/codex"
recovery_source="$recovery_dir/old-codex"
recovery_workspace="$recovery_dir/worktree"
recovery_bin="$recovery_dir/bin"
mkdir -p "$recovery_home" "$recovery_source/sessions/2026/09/23" "$recovery_workspace" "$recovery_bin"
printf '%s\n' '{"tokens":{}}' >"$recovery_home/auth.json"
printf '%s\n' '{"type":"session_meta","payload":{"session_id":"native-session-dead","cwd":"'"$recovery_workspace"'"}}' >"$recovery_source/sessions/2026/09/23/rollout-native-session-dead.jsonl"
export CODEX_HOME="$recovery_home"
export PR_GREEN_CODEX_HOME_CANDIDATES="$recovery_source:$recovery_home"
export PR_GREEN_AO_DB_PATH="$recovery_dir/ao.db"
: >"$PR_GREEN_AO_DB_PATH"
recovery_state="$recovery_dir/recovery-state.tsv"
printf '%s||1\n' "$recovery_workspace" >"$recovery_state"
export PR_GREEN_RECOVERY_STATE="$recovery_state"
cat >"$recovery_bin/sqlite3" <<'EOF'
#!/usr/bin/env bash
if [[ "$*" == *"'wa-busy'"* ]]; then
  printf '%s\n' 'worldarchitect-ai-777-deadbeef'
elif [[ "$*" == *"'wa-dead'"* ]]; then
  cat "$PR_GREEN_RECOVERY_STATE"
fi
EOF
chmod +x "$recovery_bin/sqlite3"
export PR_GREEN_RECOVERY_WORKSPACE="$recovery_workspace"
export PATH="$recovery_bin:$PATH"
ao_calls_file="$(mktemp)"
trap 'rm -rf "$recovery_dir"; rm -f "$ao_calls_file"' EXIT
ao() {
  printf '%s\n' "$*" >>"$ao_calls_file"
  if [[ "$1 $2" == "session get" ]]; then
    printf '%s\n' "$session_get_fixture"
    return 0
  fi
  case "$1 $2" in
    "status --json") printf '%s\n' '{"port":43123}' ;;
    "session ls") printf '%s\n' "$fixture" ;;
    "session restore") [[ "$restore_fail" -eq 0 ]] ;;
    "send --session") [[ "$send_fail" -eq 0 ]] ;;
    *) printf 'unexpected ao call: %s\n' "$*" >&2; return 1 ;;
  esac
}

curl() {
  printf '%s|%s|1\n' "$PR_GREEN_RECOVERY_WORKSPACE" 'native-session-dead' >"$PR_GREEN_RECOVERY_STATE"
  printf '%s\n' '{"status":"ok"}'
}

# The production helper reads only the matched AO session's runtime handle and
# asks tmux for the current pane.  Keep this fake narrow so the regression
# proves a visibly working pane is deferred before `ao send` is attempted.
tmux() {
  case "$1 $2" in
    "has-session -t") [[ "$3" == "$busy_runtime_handle" ]] ;;
    "capture-pane -p") printf '%s\n' "$busy_pane_output" ;;
    *) printf 'unexpected tmux call: %s\n' "$*" >&2; return 1 ;;
  esac
}

assert_eq() {
  [[ "$1" == "$2" ]] || { printf 'assertion failed: %s != %s\n' "$1" "$2" >&2; exit 1; }
}

success_fixture="$(mktemp)"
printf '%s\n' 'spawned session worldarchitect.ai-31 (idle) (claimed URL)' >"$success_fixture"
pr_green_spawn_output_is_success "$success_fixture"
printf '%s\n' 'spawned session worldarchitect.ai-36 (idle) (claimed https://github.com/jleechanorg/worldarchitect.ai/pull/9970)' >"$success_fixture"
pr_green_spawn_output_is_success "$success_fixture"
printf '%s\n' 'spawn acknowledgement parser accepted current Go output'
printf '%s\n' 'spawn failed: another ao spawn is in progress' >"$success_fixture"
if pr_green_spawn_output_is_success "$success_fixture"; then
  printf 'spawn acknowledgement parser accepted a failure\n' >&2
  exit 1
fi
rm -f "$success_fixture"

action_file="$(mktemp)"
pr_green_reuse_session worldarchitect.ai 123 'updated prompt' >"$action_file"
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" reused
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${#ao_calls[@]}" 2
assert_eq "${ao_calls[1]}" 'send --session wa-live --message updated prompt'
if rg -q '^spawn ' "$ao_calls_file"; then
  printf 'live-session reuse must not spawn a second session\n' >&2
  exit 1
fi

# Sessions created before the AO hook-path repair can still say idle in AO's
# database while their actual Codex pane visibly works.  Such a session must
# be deferred, never sent another prompt and never replaced by a duplicate.
fixture='{"data":[{"id":"wa-busy","displayName":"pr-777","isTerminated":false,"status":"pr_open","updatedAt":"2026-09-23T00:03:00Z"}]}'
busy_runtime_handle='worldarchitect-ai-777-deadbeef'
busy_pane_output='Working (4m 12s)\nWaiting for background terminal'
: >"$ao_calls_file"
action_file="$(mktemp)"
pr_green_reuse_session worldarchitect.ai 777 'must not queue this prompt' >"$action_file"
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" busy_deferred
if rg -q '^send ' "$ao_calls_file"; then
  printf 'busy live session must not receive a queued prompt\n' >&2
  exit 1
fi

fixture='{"data":[{"id":"wa-fallback","isTerminated":false,"status":"pr_open","updatedAt":"2026-09-23T00:02:00Z"}]}'
session_get_fixture='{"session":{"id":"wa-fallback","displayName":"pr-654","isTerminated":false,"status":"pr_open","updatedAt":"2026-09-23T00:02:00Z"}}'
: >"$ao_calls_file"
action_file="$(mktemp)"
pr_green_reuse_session worldarchitect.ai 654 'hydrated prompt' >"$action_file"
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" reused
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${ao_calls[1]}" 'session get wa-fallback -p worldarchitect.ai --json'
assert_eq "${ao_calls[2]}" 'send --session wa-fallback --message hydrated prompt'

fixture='{"data":[{"id":"wa-dead-old","displayName":"pr-321","isTerminated":true,"status":"terminated","updatedAt":"2026-09-23T00:01:00Z"},{"id":"wa-live-new","displayName":"pr-321","isTerminated":false,"status":"pr_open","updatedAt":"2026-09-23T00:00:00Z"}]}'
session_get_fixture=''
: >"$ao_calls_file"
action_file="$(mktemp)"
pr_green_reuse_session worldarchitect.ai 321 'prefer live prompt' >"$action_file"
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" reused
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${ao_calls[1]}" 'send --session wa-live-new --message prefer live prompt'

fixture='{"data":[{"id":"wa-dead","displayName":"pr-456","isTerminated":true,"status":"terminated","updatedAt":"2026-09-23T00:00:00Z"}]}'
: >"$ao_calls_file"
action_file="$(mktemp)"
pr_green_reuse_session worldarchitect.ai 456 'restore prompt' >"$action_file"
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" restored
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${#ao_calls[@]}" 4
assert_eq "${ao_calls[1]}" 'status --json'
assert_eq "${ao_calls[2]}" 'session restore wa-dead -p worldarchitect.ai'
assert_eq "${ao_calls[3]}" 'send --session wa-dead --message restore prompt'

# A successful restore followed by a rejected prompt still identifies the
# exact existing session. The caller must suppress duplicate spawn rather than
# treating it as a missing session.
send_fail=1
: >"$ao_calls_file"
set +e
pr_green_reuse_session worldarchitect.ai 456 'restore retry prompt' >/dev/null
rc=$?
set -e
send_fail=0
if [[ "$rc" -ne 2 ]]; then
  printf 'restore/send failure must return duplicate-suppression code 2 (got %s)\n' "$rc" >&2
  exit 1
fi
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${ao_calls[0]}" 'session ls -p worldarchitect.ai --include-terminated --json'
assert_eq "${ao_calls[1]}" 'session restore wa-dead -p worldarchitect.ai'
assert_eq "${ao_calls[2]}" 'send --session wa-dead --message restore retry prompt'

# A restore command can partially launch a worker before returning an error;
# never fall through to a duplicate spawn after that ambiguous outcome.
restore_fail=1
: >"$ao_calls_file"
set +e
pr_green_reuse_session worldarchitect.ai 456 'restore failure prompt' >/dev/null
rc=$?
set -e
restore_fail=0
if [[ "$rc" -ne 2 ]]; then
  printf 'restore failure must return duplicate-suppression code 2 (got %s)\n' "$rc" >&2
  exit 1
fi

fixture='{"data":[]}'
: >"$ao_calls_file"
set +e
pr_green_reuse_session worldarchitect.ai 789 'new prompt'
rc=$?
set -e
if [[ "$rc" -ne 1 ]]; then
  printf 'expected no-session lookup to return 1\n' >&2
  exit 1
fi

printf 'session reuse tests passed\n'
