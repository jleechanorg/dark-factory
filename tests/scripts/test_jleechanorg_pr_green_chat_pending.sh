#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/../../jobs/jleechanorg-pr-green/session-reuse.sh"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
export PR_GREEN_DELIVERY_STATE_DIR="$tmp"
# A transient empty AO listing must not admit a second worker when an
# initial Chat prompt still has a durable unresolved receipt.
pr_green_session_record() { return 0; }
path=$(pr_green_delivery_pending_path worldarchitect.ai 123)
printf '%s\n' '{"session":"worldarchitect.ai-65","initial":true,"text":"repair"}' > "${path}.chat"
[[ "$(pr_green_delivery_pending_status worldarchitect.ai 123)" == pending ]]
rc=0; pr_green_reuse_session worldarchitect.ai 123 repair || rc=$?
[[ "$rc" == 4 && -f "${path}.chat" ]]
# Corrupt/empty Chat ledgers are reservations too; only actual receipt
# observation may retire them.
: > "${path}.chat"
[[ "$(pr_green_delivery_pending_status worldarchitect.ai 123)" == pending ]]
rc=0; pr_green_reuse_session worldarchitect.ai 123 repair || rc=$?
[[ "$rc" == 4 && -f "${path}.chat" ]]
rm "${path}.chat"
[[ "$(pr_green_delivery_pending_status worldarchitect.ai 123)" == none ]]
rc=0; pr_green_reuse_session worldarchitect.ai 123 repair || rc=$?
[[ "$rc" == 1 ]]
printf 'PASS: Chat receipt reserves missing session and malformed ledger fails closed\n'

# Busy Chat is a deferral, never an acknowledged reuse. Preserve both genuine
# delivery success and unconfirmed failures through the shell adapter.
pr_green_session_record() { printf '%s\n' '{"id":"worldarchitect.ai-65"}'; }
pr_green_session_mode() { printf '%s\n' chat; }
pr_green_chat_delivery() { return 5; }
action=$(pr_green_reuse_session worldarchitect.ai 123 repair)
[[ "$action" == busy_deferred ]]
pr_green_chat_delivery() { return 0; }
action=$(pr_green_reuse_session worldarchitect.ai 123 repair)
[[ "$action" == reused ]]
pr_green_chat_delivery() { return 4; }
rc=0; action=$(pr_green_reuse_session worldarchitect.ai 123 repair) || rc=$?
[[ "$rc" == 4 && -z "$action" ]]
pr_green_chat_delivery() { return 6; }
action=$(pr_green_reuse_session worldarchitect.ai 123 repair)
[[ "$action" == receipt_recovered ]]
pr_green_chat_delivery() { return 1; }
rc=0; pr_green_reuse_session worldarchitect.ai 123 repair || rc=$?
[[ "$rc" == 4 ]]
printf 'PASS: Chat busy and recovered receipt accounting are distinct from delivery; helper errors fail closed\n'

# Terminated Chat keeps its exact session and provider conversation on restore.
pr_green_session_record() { printf '%s\n' '{"id":"worldarchitect.ai-65","isTerminated":true}'; }
pr_green_chat_prepare_restore() { printf 'preflight\n' >> "$tmp/calls"; printf '{}\n' > "${path}.chat"; }
pr_green_before_restore_admission() { printf 'admission\n' >> "$tmp/calls"; }
ao() { [[ "$*" == 'session restore worldarchitect.ai-65 -p worldarchitect.ai' ]] || return 99; printf 'restore\n' >> "$tmp/calls"; }
pr_green_chat_delivery() { [[ -f "$tmp/calls" ]] && grep -q '^restore$' "$tmp/calls" || return 4; rm "${path}.chat"; return 0; }
action=$(pr_green_reuse_session worldarchitect.ai 123 repair)
[[ "$action" == restored ]]
[[ "$(cat "$tmp/calls")" == $'admission\npreflight\nrestore' ]]
# Any unresolved ledger blocks restore before any lifecycle change.
rm "$tmp/calls"; : > "${path}.chat"
rc=0; pr_green_reuse_session worldarchitect.ai 123 repair || rc=$?
[[ "$rc" == 4 && ! -e "$tmp/calls" ]]; rm "${path}.chat"
# Unproven native ownership must never create or restore a worker.
pr_green_chat_prepare_restore() { return 1; }
rc=0; pr_green_reuse_session worldarchitect.ai 123 repair || rc=$?
[[ "$rc" == 3 && "$(cat "$tmp/calls")" == admission ]]
rm "$tmp/calls"
# An ambiguous same-session restore does not permit a new worker or a send.
pr_green_chat_prepare_restore() { printf '{}\n' > "${path}.chat"; }
ao() { printf 'restore-error\n' >> "$tmp/calls"; return 1; }
rc=0; pr_green_reuse_session worldarchitect.ai 123 repair || rc=$?
[[ "$rc" == 2 && -e "${path}.chat" && "$(cat "$tmp/calls")" == $'admission\nrestore-error' ]]
pr_green_session_record() { return 0; }
rc=0; pr_green_reuse_session worldarchitect.ai 123 repair || rc=$?
[[ "$rc" == 4 && -e "${path}.chat" ]]
printf 'PASS: terminated Chat restores exact owner only after admission and native preflight, unresolved and ambiguous states remain reserved\n'
