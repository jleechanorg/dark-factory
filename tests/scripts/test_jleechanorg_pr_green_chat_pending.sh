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
