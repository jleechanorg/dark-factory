#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
source "$ROOT/jobs/jleechanorg-pr-green/session-reuse.sh"
pr_green_runtime_handle() { printf '%s\n' ptyhost-v1:opaque; }
tmux() { echo 'FAIL: tmux used on native PTY' >&2; exit 99; }
pr_green_native_runtime_observation() { return 1; }
# Unknown PTY evidence must defer work rather than masquerading as idle.
pr_green_live_session_is_busy wa wa-1
if pr_green_live_codex_home wa wa-1 /work; then exit 1; fi
# Existing pending native request remains pending when no envelope is observed.
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export PR_GREEN_DELIVERY_STATE_DIR="$tmp"
printf '%s\n' '{"session_id":"wa-1","native_id":"native-session-123","workspace":"/work","envelope":"exact pending body","request_id":"r1"}' > "$tmp/wa-1.json"
pr_green_find_native_rollouts() { printf '%s\n' native-session-123; }
pr_green_delivery_ack_count() { printf '%s\n' 0; }
[[ "$(pr_green_delivery_pending_status wa 1)" == pending ]]
[[ -f "$tmp/wa-1.json" ]]
# PTY Enter-only recovery refuses before a pane or terminal input can be used.
pr_green_session_record() { printf '%s\n' '{"id":"wa-1","isTerminated":false}'; }
pr_green_session_recovery_row() { printf '%s\n' '/work|native-session-123|0'; }
CODEX_HOME="$tmp/home"; mkdir "$CODEX_HOME"; printf '{}\n' > "$CODEX_HOME/auth.json"
pr_green_live_codex_home() { printf '%s\n' "$CODEX_HOME"; }
pr_green_live_session_is_busy() { return 1; }
if pr_green_delivery_recovery_identity wa 1 wa-1 native-session-123 /work; then exit 1; fi
[[ "$(pr_green_delivery_pending_status wa 1)" == pending ]]
echo 'PASS: native PTY fails closed without tmux, missing ack preserves duplicate suppression'
# Chat never enters terminal transport or native registration.
pr_green_session_record() { printf '%s\n' '{"id":"wa-1","isTerminated":false}'; }
pr_green_session_mode() { printf '%s\n' chat; }
ao() { echo 'FAIL: unsupported Chat transport used' >&2; exit 99; }
rc=0; pr_green_reuse_session wa 1 repair >/dev/null 2>&1 || rc=$?
[[ "$rc" == 4 && -f "$tmp/wa-1.json" ]]
# Restore the real busy helper for focused native task-boundary evidence.
source "$ROOT/jobs/jleechanorg-pr-green/session-reuse.sh"
pr_green_runtime_handle() { printf '%s\n' ptyhost-v1:opaque; }
pr_green_native_runtime_observation() { jq -cn --arg home "$tmp" '{activity:"idle",nativeId:"native-session-123",home:$home}'; }
pr_green_session_recovery_row() { printf '%s\n' '/work|native-session-123|0'; }
pr_green_find_native_rollouts() { printf 'native-session-123\nnative-session-123\t%s\trollout.jsonl\n' "$tmp"; }
printf '%s\n' '{"type":"event_msg","payload":{"type":"task_started"}}' > "$tmp/rollout.jsonl"
pr_green_live_session_is_busy wa wa-1
printf '%s\n' '{"type":"event_msg","payload":{"type":"task_complete"}}' >> "$tmp/rollout.jsonl"
if pr_green_live_session_is_busy wa wa-1; then echo 'FAIL: exact completed task should permit idle' >&2; exit 1; fi
printf '{"type":' >> "$tmp/rollout.jsonl"
pr_green_live_session_is_busy wa wa-1
: > "$tmp/rollout.jsonl"
pr_green_live_session_is_busy wa wa-1
# A stale copy in another account profile cannot make the verified owner busy.
mkdir "$tmp/stale-profile"
printf '%s\n' '{"type":"event_msg","payload":{"type":"task_started"}}' > "$tmp/stale-profile/rollout.jsonl"
printf '%s\n' '{"type":"event_msg","payload":{"type":"task_complete"}}' > "$tmp/rollout.jsonl"
pr_green_find_native_rollouts() { printf 'native-session-123\nnative-session-123\t%s/stale-profile\trollout.jsonl\nnative-session-123\t%s\trollout.jsonl\n' "$tmp" "$tmp"; }
if pr_green_live_session_is_busy wa wa-1; then echo 'FAIL: stale copied rollout overrides verified live profile' >&2; exit 1; fi
# A live owner's started boundary still wins over an idle foreign copy.
printf '%s\n' '{"type":"event_msg","payload":{"type":"task_started"}}' > "$tmp/rollout.jsonl"
printf '%s\n' '{"type":"event_msg","payload":{"type":"task_complete"}}' > "$tmp/stale-profile/rollout.jsonl"
pr_green_live_session_is_busy wa wa-1
# No exact live-profile rollout is unavailable evidence, not idle.
pr_green_native_runtime_observation() { printf '%s\n' '{"activity":"idle","nativeId":"native-session-123","home":"/unavailable-profile"}'; }
pr_green_live_session_is_busy wa wa-1
# Legacy tmux dispatch remains on its existing path.
pr_green_runtime_handle() { printf '%s\n' legacy-runtime; }
pr_green_tmux_socket_for_handle() { printf 'default\n'; }
tmux() { case "$1" in has-session) return 0;; capture-pane) printf '%s\n' 'Working (2s)';; *) return 99;; esac; }
pr_green_live_session_is_busy wa wa-1
echo 'PASS: Chat pending preserved, native started/completed/partial/missing boundaries fenced, legacy busy detection preserved'
