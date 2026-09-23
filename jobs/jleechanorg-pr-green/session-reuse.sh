#!/usr/bin/env bash

# AO session reuse helpers for the PR-green sweep. These functions deliberately
# use AO's project-scoped JSON API rather than scraping daemon internals.

# Recognize both legacy and current Go AO spawn acknowledgements. The Go CLI
# exits after claiming a PR and reports either a generic "claimed URL" or the
# actual claimed pull-request URL, rather than the older SESSION= or Worktree
# markers.
pr_green_spawn_output_is_success() {
  grep -Eq 'SESSION=wa-[0-9]+|created and claimed PR|Worktree: |spawned session [^[:space:]]+ .*claimed (URL|https?://)' "$1"
}

# Return the newest session whose display name is the stable PR key used by the
# scheduler (for example, pr-9757). The complete JSON object is returned so
# callers can distinguish live and terminated sessions.
pr_green_session_record() {
  local project_id="$1"
  local pr_number="$2"
  local list_json direct ids id session_json session_record
  local -a records=()

  list_json="$(ao session ls -p "$project_id" --include-terminated --json 2>/dev/null || true)"
  [[ -n "$list_json" ]] || return 1

  # Some Go AO versions omit displayName from session ls. Use it when present
  # and avoid extra RPCs; otherwise hydrate each project-scoped session with the
  # supported session get JSON endpoint instead of reading AO's SQLite files.
  direct="$(jq -c --arg name "pr-${pr_number}" '
    def choose:
      (map(select((.isTerminated // false) | not))
       | sort_by(.updatedAt // .createdAt // "") | last) //
      (sort_by(.updatedAt // .createdAt // "") | last) // empty;
    [.data[]? | select(.displayName == $name)] | choose' <<<"$list_json" 2>/dev/null || true)"
  if [[ -n "$direct" && "$direct" != "null" ]]; then
    printf '%s\n' "$direct"
    return 0
  fi

  ids="$(jq -r '.data[]?.id // empty' <<<"$list_json" 2>/dev/null || true)"
  while IFS= read -r id; do
    [[ -n "$id" ]] || continue
    session_json="$(ao session get "$id" -p "$project_id" --json 2>/dev/null || true)"
    session_record="$(jq -c --arg name "pr-${pr_number}" 'select(.session.displayName == $name) | .session' <<<"$session_json" 2>/dev/null || true)"
    [[ -n "$session_record" && "$session_record" != "null" ]] || continue
    records+=("$session_record")
  done <<<"$ids"

  if ((${#records[@]})); then
    printf '%s\n' "${records[@]}" | jq -s -c '
      def choose:
        (map(select((.isTerminated // false) | not))
         | sort_by(.updatedAt // .createdAt // "") | last) //
        (sort_by(.updatedAt // .createdAt // "") | last) // empty;
      choose'
  fi
}

# Return the tmux runtime handle for exactly one AO session. `ao session get`
# confirms the session identity, but intentionally omits operational handles
# from its public JSON read model.  The scheduler may therefore make this
# narrowly-scoped, read-only SQLite lookup against AO's own state store.
pr_green_runtime_handle() {
  local project_id="$1"
  local session_id="$2"
  local db escaped_project escaped_session

  # AO ids and project ids are generated identifiers. Refuse anything else so
  # no value obtained from a daemon response can become SQL syntax.
  [[ "$project_id" =~ ^[[:alnum:]._-]+$ && "$session_id" =~ ^[[:alnum:]._-]+$ ]] || return 1
  command -v sqlite3 >/dev/null 2>&1 || return 1
  db="${PR_GREEN_AO_DB_PATH:-$HOME/.ao/data/ao.db}"
  [[ -r "$db" ]] || return 1
  escaped_project="${project_id//\'/\'\'}"
  escaped_session="${session_id//\'/\'\'}"
  sqlite3 -noheader "$db" \
    "SELECT runtime_handle_id FROM sessions WHERE id = '$escaped_session' AND project_id = '$escaped_project' LIMIT 1;" \
    2>/dev/null | head -n 1
}

# Return success only when the exact tmux runtime for a live AO session is
# visibly still working.  This avoids queuing a second Codex turn when legacy
# sessions have stale AO activity_state=idle because they started before the
# hook PATH repair.  If the handle/pane cannot be inspected, preserve ordinary
# AO reuse rather than treating an unknown state as permanently busy.
pr_green_live_session_is_busy() {
  local project_id="$1"
  local session_id="$2"
  local runtime_handle pane

  runtime_handle="$(pr_green_runtime_handle "$project_id" "$session_id" || true)"
  [[ -n "$runtime_handle" ]] || return 1
  tmux has-session -t "$runtime_handle" 2>/dev/null || return 1
  pane="$(tmux capture-pane -p -t "$runtime_handle" -S -80 2>/dev/null || true)"
  grep -Eq 'Working \(|Waiting for agents|Waiting for background terminal' <<<"$pane"
}

# Reuse a live session, or restore a terminated one and then send it the
# current prompt. Return codes:
#   0 — live/restore prompt sent, or a visibly busy live session was deferred
#   1 — no matching session, or restoration failed; caller may spawn
#   2 — matching live session could not accept the prompt; caller must not
#       spawn a duplicate fleet member during this run
pr_green_reuse_session() {
  local project_id="$1"
  local pr_number="$2"
  local prompt="$3"
  local record session_id terminated restore_output

  record="$(pr_green_session_record "$project_id" "$pr_number" 2>/dev/null || true)"
  [[ -n "$record" ]] || return 1

  session_id="$(jq -r '.id // empty' <<<"$record")"
  [[ -n "$session_id" ]] || return 1
  terminated="$(jq -r 'if (.isTerminated // false) then "true" else "false" end' <<<"$record")"

  if [[ "$terminated" == "true" ]]; then
    if ! restore_output="$(ao session restore "$session_id" -p "$project_id" 2>&1)"; then
      printf '%s\n' "AO session $session_id could not be restored: $restore_output" >&2
      return 1
    fi
    printf '%s\n' "restored"
  else
    if pr_green_live_session_is_busy "$project_id" "$session_id"; then
      printf '%s\n' "busy_deferred"
      return 0
    fi
    printf '%s\n' "reused"
  fi

  if ! ao send --session "$session_id" --message "$prompt" >/dev/null 2>&1; then
    if [[ "$terminated" == "true" ]]; then
      printf '%s\n' "AO restored session $session_id did not accept the prompt" >&2
      return 1
    fi
    printf '%s\n' "AO live session $session_id did not accept the prompt; suppressing duplicate spawn" >&2
    return 2
  fi
}
