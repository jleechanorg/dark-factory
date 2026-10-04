#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/../../jobs/jleechanorg-pr-green/session-reuse.sh"
pr_green_daemon_tmux_socket() { printf 'ao\n'; }
named_exists=1; legacy=0
calls=$(mktemp); trap 'rm -f "$calls"' EXIT
# Exact handle probes must not match another session by prefix.
tmux() {
  printf '%s\n' "$*" >> "$calls"
  case "$*" in
    '-L ao has-session -t =worker-1') [[ "$named_exists" == 1 ]];;
    'has-session -t =worker-1') [[ "$legacy" == 1 ]];;
    '-L ao capture-pane -p -t %2') printf 'named_exists\n';;
    'capture-pane -p -t %2') printf 'legacy\n';;
    *) return 99;;
  esac
}
[[ "$(pr_green_tmux_socket_for_handle worker-1)" == ao ]]
[[ "$(pr_green_tmux_at ao capture-pane -p -t %2)" == named_exists ]]
named_exists=0; legacy=1
[[ "$(pr_green_tmux_socket_for_handle worker-1)" == default ]]
[[ "$(pr_green_tmux_at default capture-pane -p -t %2)" == legacy ]]
named_exists=1
if pr_green_tmux_socket_for_handle worker-1; then echo 'FAIL ambiguous socket'; exit 1; fi
named_exists=0; legacy=0
if pr_green_tmux_socket_for_handle worker-1; then echo 'FAIL missing runtime'; exit 1; fi
if pr_green_tmux_socket_for_handle 'ptyhost-v1:opaque'; then exit 1; fi
if pr_green_tmux_at '../bad' send-keys -t %2 Enter; then exit 1; fi
printf 'PASS: named_exists/default routing, exact target and ambiguous/missing rejection\n'
