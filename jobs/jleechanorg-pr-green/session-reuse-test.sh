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
native_ack_file=''
recovery_dir="$(mktemp -d)"
recovery_home="$recovery_dir/codex"
recovery_source="$recovery_dir/old-codex"
recovery_workspace="$recovery_dir/worktree"
recovery_bin="$recovery_dir/bin"
mkdir -p "$recovery_home/sessions/2026/09/23" "$recovery_source/sessions/2026/09/23" "$recovery_workspace" "$recovery_bin"
printf '%s\n' '{"tokens":{}}' >"$recovery_home/auth.json"
printf '%s\n' '{"type":"session_meta","payload":{"session_id":"native-session-dead","cwd":"'"$recovery_workspace"'"},"timestamp":"2026-09-23T16:01:00Z"}' >"$recovery_source/sessions/2026/09/23/rollout-native-session-dead.jsonl"
live_rollout="$recovery_home/sessions/2026/09/23/rollout-native-session-live.jsonl"
fallback_rollout="$recovery_home/sessions/2026/09/23/rollout-native-session-fallback.jsonl"
live_new_rollout="$recovery_home/sessions/2026/09/23/rollout-native-session-live-new.jsonl"
for rollout in "$live_rollout" "$fallback_rollout" "$live_new_rollout"; do
  native_id="${rollout##*rollout-}"
  native_id="${native_id%.jsonl}"
  printf '%s\n' '{"type":"session_meta","payload":{"session_id":"'"$native_id"'","cwd":"'"$recovery_workspace"'"}}' >"$rollout"
  printf '%s\n' '{"type":"response_item","payload":{"role":"user","content":[]},"timestamp":"2026-09-23T15:51:12Z"}' >>"$rollout"
done
export CODEX_HOME="$recovery_home"
export PR_GREEN_CODEX_HOME_CANDIDATES="$recovery_source:$recovery_home"
export PR_GREEN_AO_DB_PATH="$recovery_dir/ao.db"
export PR_GREEN_AO_RUN_FILE="$recovery_dir/running.json"
: >"$PR_GREEN_AO_DB_PATH"
printf '%s\n' '{"pid":1234,"port":43123}' >"$PR_GREEN_AO_RUN_FILE"
recovery_state="$recovery_dir/recovery-state.tsv"
printf '%s||1|2026-09-23T16:00:00Z\n' "$recovery_workspace" >"$recovery_state"
export PR_GREEN_RECOVERY_STATE="$recovery_state"
export PR_GREEN_DELIVERY_STATE_DIR="$recovery_dir/delivery"
cat >"$recovery_bin/sqlite3" <<'EOF'
#!/usr/bin/env bash
if [[ "$*" == *"FROM threads"* ]]; then
  if [[ "${PR_GREEN_TEST_INDEXED_ROWS:-0}" == 1 ]]; then
    printf '%s\n' "${PR_GREEN_INDEXED_ROWS:-}"
    exit 0
  fi
  exit 1
elif [[ "$*" == *"SELECT runtime_handle_id"* && "$*" == *"'wa-busy'"* ]]; then
  printf '%s\n' 'worldarchitect-ai-777-deadbeef'
elif [[ "$*" == *"SELECT runtime_handle_id"* && "$*" == *"'wa-live'"* ]]; then
  printf '%s\n' 'worldarchitect-ai-123-native'
elif [[ "$*" == *"SELECT runtime_handle_id"* && "$*" == *"'wa-fallback'"* ]]; then
  printf '%s\n' 'worldarchitect-ai-654-native'
elif [[ "$*" == *"SELECT runtime_handle_id"* && "$*" == *"'wa-live-new'"* ]]; then
  printf '%s\n' 'worldarchitect-ai-321-native'
elif [[ "$*" == *"'wa-live'"* ]]; then
  printf '%s|native-session-live|0|2026-09-23T15:51:11Z\n' "$PR_GREEN_RECOVERY_WORKSPACE"
elif [[ "$*" == *"'wa-fallback'"* ]]; then
  printf '%s|native-session-fallback|0|2026-09-23T15:51:11Z\n' "$PR_GREEN_RECOVERY_WORKSPACE"
elif [[ "$*" == *"'wa-live-new'"* ]]; then
  printf '%s|native-session-live-new|0|2026-09-23T15:51:11Z\n' "$PR_GREEN_RECOVERY_WORKSPACE"
elif [[ "$*" == *"'wa-dead'"* ]]; then
  cat "$PR_GREEN_RECOVERY_STATE"
elif [[ "$*" == *"'wa-dead-missing'"* ]]; then
  printf '%s\n' "$PR_GREEN_RECOVERY_WORKSPACE/missing||1"
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
    "send --session")
      [[ "$send_fail" -eq 0 ]] || return 1
      if [[ -n "$native_ack_file" ]]; then
        jq -cn --arg prompt "$5" '{type:"response_item",payload:{role:"user",content:[{type:"input_text",text:$prompt}]},timestamp:"2026-09-23T16:00:00Z"}' >>"$native_ack_file"
      fi
      ;;
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

# Transport tests provide the exact native identity rows above; the separate
# live-process identity contract is covered by native-recovery tests.
pr_green_preserve_live_native_conversation() { return 0; }

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
native_ack_file="$live_rollout"
pr_green_reuse_session worldarchitect.ai 123 'updated prompt' >"$action_file"
native_ack_file=''
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" reused
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${#ao_calls[@]}" 2
[[ "${ao_calls[1]}" == 'send --session wa-live --message Read and execute '* ]]
if rg -q '^spawn ' "$ao_calls_file"; then
  printf 'live-session reuse must not spawn a second session\n' >&2
  exit 1
fi

# A successful AO transport response without the exact prompt appearing as a
# new native user turn is unconfirmed; it must not be reported as reused.
: >"$ao_calls_file"
set +e
pr_green_reuse_session worldarchitect.ai 123 'transport acknowledgement missing' >/dev/null
rc=$?
set -e
if [[ "$rc" -ne 4 ]]; then
  printf 'missing native acknowledgement must return delivery-unconfirmed code 4 (got %s)\n' "$rc" >&2
  exit 1
fi

# A later invocation must not append a new request while the old request is
# unresolved. A delayed acknowledgement for the old envelope is not an
# acknowledgement for a future request; only after it is observed may the
# next request be sent with a distinct nonce.
pending_path="$(pr_green_delivery_pending_path worldarchitect.ai 123)"
pending_envelope="$(jq -r '.envelope' "$pending_path")"
: >"$ao_calls_file"
set +e
pr_green_reuse_session worldarchitect.ai 123 'new prompt before old ack' >/dev/null
rc=$?
set -e
if [[ "$rc" -ne 4 ]]; then
  printf 'unresolved old delivery must suppress a new prompt (got %s)\n' "$rc" >&2
  exit 1
fi
if rg -q '^send ' "$ao_calls_file"; then
  printf 'unresolved old delivery appended a new prompt\n' >&2
  exit 1
fi
jq -cn --arg prompt "$pending_envelope" '{type:"response_item",payload:{role:"user",content:[{type:"input_text",text:$prompt}]},timestamp:"2026-09-23T16:01:00Z"}' >>"$live_rollout"
native_ack_file="$live_rollout"
PR_GREEN_TEST_DELIVERY_ID='new-delivery-id'
: >"$ao_calls_file"
action_file="$(mktemp)"
pr_green_reuse_session worldarchitect.ai 123 'new prompt after old ack' >"$action_file"
action="$(<"$action_file")"
rm -f "$action_file"
native_ack_file=''
unset PR_GREEN_TEST_DELIVERY_ID
assert_eq "$action" reused
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${#ao_calls[@]}" 2
[[ "${ao_calls[1]}" == 'send --session wa-live --message Read and execute '* ]]
[[ ! -e "$pending_path" ]]

# The short transport envelope must point at an immutable, mode-600 brief that
# preserves the complete prompt byte-for-byte; the native ack proves only the
# pointer envelope, while the brief is the semantic instruction source.
brief_path="$recovery_dir/delivery/worldarchitect.ai-123.new-delivery-id.brief"
assert_eq "$(<"$brief_path")" 'new prompt after old ack'
assert_eq "$(stat -c '%a' "$brief_path")" 600
[[ "${ao_calls[1]}" == *"$brief_path"*PR_GREEN_DELIVERY_ID:new-delivery-id* ]]

# A pending record bound to another native identity is never migrated or sent
# through the current AO session; the record remains for a later exact match.
pending_path="$(pr_green_delivery_pending_path worldarchitect.ai 123)"
jq -cn --arg workspace "$recovery_workspace" \
  '{project_id:"worldarchitect.ai",pr_number:123,session_id:"wa-live",native_id:"wrong-native-id",workspace:$workspace,prompt:"identity mismatch",envelope:"identity mismatch [PR_GREEN_DELIVERY_ID:wrong-id]",request_id:"wrong-id",created_at:1}' \
  >"$pending_path"
: >"$ao_calls_file"
set +e
pr_green_reuse_session worldarchitect.ai 123 'must not cross native identity' >/dev/null
rc=$?
set -e
assert_eq "$rc" 4
if rg -q '^send ' "$ao_calls_file"; then
  printf 'native identity mismatch must not send through the live session\n' >&2
  exit 1
fi
rm -f -- "$pending_path"

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
native_ack_file="$fallback_rollout"
pr_green_reuse_session worldarchitect.ai 654 'hydrated prompt' >"$action_file"
native_ack_file=''
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" reused
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${ao_calls[1]}" 'session get wa-fallback -p worldarchitect.ai --json'
[[ "${ao_calls[2]}" == 'send --session wa-fallback --message Read and execute '* ]]

fixture='{"data":[{"id":"wa-dead-old","displayName":"pr-321","isTerminated":true,"status":"terminated","updatedAt":"2026-09-23T00:01:00Z"},{"id":"wa-live-new","displayName":"pr-321","isTerminated":false,"status":"pr_open","updatedAt":"2026-09-23T00:00:00Z"}]}'
session_get_fixture=''
: >"$ao_calls_file"
action_file="$(mktemp)"
native_ack_file="$live_new_rollout"
pr_green_reuse_session worldarchitect.ai 321 'prefer live prompt' >"$action_file"
native_ack_file=''
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" reused
mapfile -t ao_calls <"$ao_calls_file"
[[ "${ao_calls[1]}" == 'send --session wa-live-new --message Read and execute '* ]]

fixture='{"data":[{"id":"wa-dead","displayName":"pr-456","isTerminated":true,"status":"terminated","updatedAt":"2026-09-23T00:00:00Z"}]}'
restore_admission=1
pr_green_before_restore_admission() { return "$restore_admission"; }
: >"$ao_calls_file"
set +e
pr_green_reuse_session worldarchitect.ai 456 'restore must respect cap' >/dev/null
rc=$?
set -e
[[ "$rc" -eq 2 ]] || { printf 'restore admission hook must suppress capped restore (got %s)\n' "$rc" >&2; exit 1; }
if rg -q '^session restore ' "$ao_calls_file"; then
  printf 'capped restore reached AO restore\n' >&2
  exit 1
fi
restore_admission=0
: >"$ao_calls_file"
action_file="$(mktemp)"
native_ack_file="$recovery_home/sessions/2026/09/23/rollout-native-session-dead.jsonl"
pr_green_reuse_session worldarchitect.ai 456 'restore prompt' >"$action_file"
native_ack_file=''
action="$(<"$action_file")"
rm -f "$action_file"
assert_eq "$action" restored
mapfile -t ao_calls <"$ao_calls_file"
assert_eq "${#ao_calls[@]}" 3
assert_eq "${ao_calls[1]}" 'session restore wa-dead -p worldarchitect.ai'
[[ "${ao_calls[2]}" == 'send --session wa-dead --message Read and execute '* ]]

# Legacy full-prompt pending state is upgraded only after the exact terminated
# session is restored. The original prompt is retained in the brief and the
# resumed native conversation receives one short pointer with the same nonce.
pending_path="$(pr_green_delivery_pending_path worldarchitect.ai 456)"
jq -cn --arg workspace "$recovery_workspace" \
  '{project_id:"worldarchitect.ai",pr_number:456,session_id:"wa-dead",native_id:"native-session-dead",workspace:$workspace,prompt:"legacy prompt",envelope:"legacy prompt [PR_GREEN_DELIVERY_ID:legacy-id]",request_id:"legacy-id",created_at:1}' \
  >"$pending_path"
: >"$ao_calls_file"
action_file="$(mktemp)"
native_ack_file="$recovery_home/sessions/2026/09/23/rollout-native-session-dead.jsonl"
pr_green_reuse_session worldarchitect.ai 456 'replacement prompt must not overwrite legacy request' >"$action_file"
native_ack_file=''
action="$(<"$action_file")"
rm -f -- "$action_file"
assert_eq "$action" restored
legacy_brief="$recovery_dir/delivery/worldarchitect.ai-456.legacy-id.brief"
assert_eq "$(<"$legacy_brief")" 'legacy prompt'
mapfile -t ao_calls <"$ao_calls_file"
[[ "${ao_calls[2]}" == *"send --session wa-dead --message Read and execute $legacy_brief. [PR_GREEN_DELIVERY_ID:legacy-id]" ]]
[[ ! -e "$pending_path" ]]

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
[[ "${ao_calls[2]}" == 'send --session wa-dead --message Read and execute '* ]]

# A restore command can fail after partially launching the existing worker;
# never fall through to a duplicate spawn.
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

# Missing/ambiguous native history is a recovery block, not a send failure.
# It must suppress duplicates without counting an inference attempt.
fixture='{"data":[{"id":"wa-dead-missing","displayName":"pr-457","isTerminated":true,"status":"terminated","updatedAt":"2026-09-23T00:00:00Z"}]}'
: >"$ao_calls_file"
set +e
pr_green_reuse_session worldarchitect.ai 457 'must not send without history' >/dev/null
rc=$?
set -e
if [[ "$rc" -ne 3 ]]; then
  printf 'missing native history must return recovery-blocked code 3 (got %s)\n' "$rc" >&2
  exit 1
fi
if rg -q 'session restore|^send ' "$ao_calls_file"; then
  printf 'recovery-blocked session must not restore or send\n' >&2
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

# The Codex thread registry is the bounded fast path when its schema/query is
# available. Its rows still require exact rollout metadata identity, and an
# equal timestamp must fail closed rather than guessing a conversation.
indexed_home="$recovery_dir/indexed-codex"
mkdir -p "$indexed_home/sessions/2026/09/23"
: >"$indexed_home/state_5.sqlite"
indexed_id='native-session-indexed'
indexed_rollout="$indexed_home/registry-rollout-$indexed_id.jsonl"
printf '%s\n' '{"type":"session_meta","payload":{"session_id":"'"$indexed_id"'","cwd":"'"$recovery_workspace"'"},"timestamp":"2026-09-23T16:02:00Z"}' >"$indexed_rollout"
export PR_GREEN_CODEX_HOME_CANDIDATES="$indexed_home"
export PR_GREEN_TEST_INDEXED_ROWS=1
export PR_GREEN_INDEXED_ROWS="$indexed_id"$'\t'"$indexed_rollout"$'\t'1727107320$'\t'1727107320
indexed_result="$(pr_green_find_native_rollouts "$recovery_workspace" '2026-09-23T16:00:00Z' "$indexed_id")"
assert_eq "$(printf '%s\n' "$indexed_result" | sed -n '1p')" "$indexed_id"
[[ "$indexed_result" == *$'\t'"$indexed_home"$'\t'"registry-rollout-$indexed_id.jsonl" ]] || {
  printf 'indexed lookup did not return the exact rollout mapping\n' >&2
  exit 1
}

ambiguous_a='native-session-ambiguous-a'
ambiguous_b='native-session-ambiguous-b'
for ambiguous_id in "$ambiguous_a" "$ambiguous_b"; do
  ambiguous_rollout="$indexed_home/sessions/2026/09/23/rollout-$ambiguous_id.jsonl"
  printf '%s\n' '{"type":"session_meta","payload":{"session_id":"'"$ambiguous_id"'","cwd":"'"$recovery_workspace"'"},"timestamp":"2026-09-23T16:03:00Z"}' >"$ambiguous_rollout"
done
export PR_GREEN_INDEXED_ROWS="$ambiguous_a"$'\t'"$indexed_home/sessions/2026/09/23/rollout-$ambiguous_a.jsonl"$'\t'1727107380$'\t'1727107380
export PR_GREEN_INDEXED_ROWS+=$'\n'"$ambiguous_b"$'\t'"$indexed_home/sessions/2026/09/23/rollout-$ambiguous_b.jsonl"$'\t'1727107380$'\t'1727107380
if pr_green_find_native_rollouts "$recovery_workspace" '' >/dev/null 2>&1; then
  printf 'indexed lookup guessed across an equal timestamp\n' >&2
  exit 1
fi

indexed_bad='native-session-indexed-bad'
indexed_bad_rollout="$indexed_home/sessions/2026/09/23/rollout-$indexed_bad.jsonl"
printf '%s\n' '{"type":"session_meta","payload":{"session_id":"native-session-other","cwd":"'"$recovery_workspace"'"},"timestamp":"2026-09-23T16:04:00Z"}' >"$indexed_bad_rollout"
export PR_GREEN_INDEXED_ROWS="$indexed_bad"$'\t'"$indexed_bad_rollout"$'\t'1727107440$'\t'1727107440
if pr_green_find_native_rollouts "$recovery_workspace" '' "$indexed_bad" >/dev/null 2>&1; then
  printf 'indexed lookup accepted a mismatched session identity\n' >&2
  exit 1
fi

indexed_old='native-session-indexed-old'
indexed_old_rollout="$indexed_home/sessions/2026/09/23/rollout-$indexed_old.jsonl"
printf '%s\n' '{"type":"session_meta","payload":{"session_id":"'"$indexed_old"'","cwd":"'"$recovery_workspace"'"},"timestamp":"2026-09-23T15:00:00Z"}' >"$indexed_old_rollout"
export PR_GREEN_INDEXED_ROWS="$indexed_old"$'\t'"$indexed_old_rollout"$'\t'1727103600$'\t'1727103600
if pr_green_find_native_rollouts "$recovery_workspace" '2026-09-23T16:00:00Z' "$indexed_old" >/dev/null 2>&1; then
  printf 'indexed lookup accepted a rollout older than the creation cutoff\n' >&2
  exit 1
fi

# A missing/unsupported registry schema keeps the legacy scan available for
# older Codex homes; this is the only condition that permits that fallback.
unset PR_GREEN_TEST_INDEXED_ROWS PR_GREEN_INDEXED_ROWS
export PR_GREEN_CODEX_HOME_CANDIDATES="$recovery_source:$recovery_home"
fallback_result="$(pr_green_find_native_rollouts "$recovery_workspace" '2026-09-23T16:00:00Z' 'native-session-dead')"
[[ "$fallback_result" == *'native-session-dead'* ]] || {
  printf 'registry-unavailable fallback did not recover the legacy rollout\n' >&2
  exit 1
}

printf 'session reuse tests passed\n'
