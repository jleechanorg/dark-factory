#!/usr/bin/env bash
# Activity Updater Hook for Agent Orchestrator
#
# Records Claude Code lifecycle events to {workspace}/.ao/activity.jsonl so
# the dashboard / lifecycle reducer derives activity state from authoritative
# platform events instead of regex over rendered terminal output. (#1941)

set -uo pipefail

input=$(cat)

if command -v jq &>/dev/null; then
  event=$(printf '%s' "$input" | jq -r '.hook_event_name // empty')
  notif_type=$(printf '%s' "$input" | jq -r '.notification_type // empty')
  tool_name=$(printf '%s' "$input" | jq -r '.tool_name // empty')
  error_type=$(printf '%s' "$input" | jq -r '.error_type // empty')
else
  event=$(printf '%s' "$input" | grep -o '"hook_event_name"[[:space:]]*:[[:space:]]*"[^"]*"' | cut -d'"' -f4)
  notif_type=$(printf '%s' "$input" | grep -o '"notification_type"[[:space:]]*:[[:space:]]*"[^"]*"' | cut -d'"' -f4)
  tool_name=$(printf '%s' "$input" | grep -o '"tool_name"[[:space:]]*:[[:space:]]*"[^"]*"' | cut -d'"' -f4)
  error_type=$(printf '%s' "$input" | grep -o '"error_type"[[:space:]]*:[[:space:]]*"[^"]*"' | cut -d'"' -f4)
fi

state=""
trigger=""
case "$event" in
  SessionStart|Stop|SubagentStop)
    state="ready"
    trigger="$event"
    ;;
  UserPromptSubmit|PreToolUse|PostToolUse|PostToolUseFailure|PreCompact|PostCompact|SubagentStart|PostToolBatch)
    state="active"
    trigger="$event"
    ;;
  PermissionRequest)
    state="waiting_input"
    if [[ -n "$tool_name" ]]; then
      trigger="PermissionRequest ($tool_name)"
    else
      trigger="PermissionRequest"
    fi
    ;;
  Notification)
    if [[ "$notif_type" == "permission_prompt" || "$notif_type" == "idle_prompt" ]]; then
      state="waiting_input"
      trigger="Notification ($notif_type)"
    else
      # auth_success / elicitation_* / unrecognized — not an activity transition
      echo '{}'
      exit 0
    fi
    ;;
  StopFailure)
    state="blocked"
    if [[ -n "$error_type" ]]; then
      trigger="StopFailure ($error_type)"
    else
      trigger="StopFailure"
    fi
    ;;
  *)
    echo '{}'
    exit 0
    ;;
esac

workspace="${CLAUDE_PROJECT_DIR:-$(pwd)}"
log_dir="$workspace/.ao"
log_file="$log_dir/activity.jsonl"

mkdir -p "$log_dir" 2>/dev/null || { echo '{}'; exit 0; }

# Node is a hard runtime dep of Claude Code, so node -p is always available
# and gives millisecond-precision ISO timestamps matching the rest of the
# activity-JSONL log. Fall back to seconds-precision date for the unlikely
# case where node is unavailable (still valid ISO 8601).
ts=$(node -p 'new Date().toISOString()' 2>/dev/null || date -u +"%Y-%m-%dT%H:%M:%SZ")

# Escape JSON-special characters in the trigger value. Triggers are bounded
# today to event/tool/error names (no control chars in practice) but escape
# defensively — \ and " for content, plus the five common control chars
# (\n \r \t \b \f) so the JSONL line stays parseable for any future
# trigger source. Matches what Node's JSON.stringify produces in the .cjs
# variant so both implementations stay in lockstep.
escape_json() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  s="${s//$'\n'/\\n}"
  s="${s//$'\r'/\\r}"
  s="${s//$'\t'/\\t}"
  s="${s//$'\b'/\\b}"
  s="${s//$'\f'/\\f}"
  printf '%s' "$s"
}

if [[ "$state" == "waiting_input" || "$state" == "blocked" ]]; then
  esc_trigger=$(escape_json "$trigger")
  printf '{"ts":"%s","state":"%s","source":"hook","trigger":"%s"}\n' "$ts" "$state" "$esc_trigger" >> "$log_file"
else
  printf '{"ts":"%s","state":"%s","source":"hook"}\n' "$ts" "$state" >> "$log_file"
fi

echo '{}'
exit 0
