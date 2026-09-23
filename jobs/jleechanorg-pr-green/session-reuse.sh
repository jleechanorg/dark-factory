#!/usr/bin/env bash

# AO session reuse helpers for the PR-green sweep. These functions deliberately
# use AO's project-scoped JSON API rather than scraping daemon internals.

# AO resolves a project's environment when a session is spawned or restored.
# Keep the repair job's Codex account binding in that project config as well as
# in the scheduler environment; otherwise AO can launch a worker with its
# registered project environment and silently fall back to the operator's
# default account. `set-config` replaces the complete config, so preserve all
# existing fields and verify the resulting project before proceeding.
pr_green_extract_codex_home() {
  jq -r '
    if ((.env // {}) | type) == "object" then (.env.CODEX_HOME // "")
    elif ((.env // []) | type) == "array" then
      ((.env // [])[] | select(startswith("CODEX_HOME=")) | sub("^CODEX_HOME="; "")) // ""
    else "" end
  ' <<<"$1" 2>/dev/null || true
}

pr_green_ensure_codex_scope() {
  local project_id="$1" codex_home="${CODEX_HOME:-}"
  local project_json config_json existing_home updated_config verified_config verified_home
  [[ -n "$codex_home" && -d "$codex_home" && -s "$codex_home/auth.json" ]] || {
    echo "PR_GREEN CODEX_HOME is not an existing authenticated scope: $codex_home" >&2
    return 1
  }
  project_json="$(ao project get "$project_id" --json 2>/dev/null || true)"
  [[ -n "$project_json" ]] || return 1
  config_json="$(jq -c '.project.config // {}' <<<"$project_json" 2>/dev/null || true)"
  [[ -n "$config_json" && "$config_json" != "null" ]] || return 1
  existing_home="$(pr_green_extract_codex_home "$config_json")"
  if [[ "$existing_home" == "$codex_home" ]]; then
    return 0
  fi
  updated_config="$(jq -c --arg codex_home "$codex_home" '
    if ((.env // {}) | type) == "object" then
      .env = ((.env // {}) + {CODEX_HOME: $codex_home})
    elif ((.env // []) | type) == "array" then
      .env = ((.env // []) | map(select(startswith("CODEX_HOME=") | not)) + ["CODEX_HOME=" + $codex_home])
    else
      .env = {CODEX_HOME: $codex_home}
    end
  ' <<<"$config_json" 2>/dev/null || true)"
  [[ -n "$updated_config" ]] || return 1
  ao project set-config "$project_id" --config-json "$updated_config" --json >/dev/null 2>&1 || return 1
  verified_config="$(ao project get "$project_id" --json 2>/dev/null | jq -c '.project.config // {}' 2>/dev/null || true)"
  verified_home="$(pr_green_extract_codex_home "$verified_config")"
  [[ "$verified_home" == "$codex_home" ]]
}

# Read the daemon's own session row only to recover the exact workspace binding
# that is not exposed by `ao session get`. The write path below remains the
# supported AO activity API; this lookup is deliberately project-scoped and
# fails closed when the local state store is unavailable or malformed.
pr_green_session_recovery_row() {
  local project_id="$1"
  local session_id="$2"
  local db
  [[ "$project_id" =~ ^[[:alnum:]._-]+$ && "$session_id" =~ ^[[:alnum:]._-]+$ ]] || return 1
  command -v sqlite3 >/dev/null 2>&1 || return 1
  db="${PR_GREEN_AO_DB_PATH:-$HOME/.ao/data/ao.db}"
  [[ -r "$db" ]] || return 1
  sqlite3 -noheader "$db" \
    "SELECT workspace_path || '|' || agent_session_id || '|' || is_terminated || '|' || created_at FROM sessions WHERE id = '${session_id//\'/\'\'}' AND project_id = '${project_id//\'/\'\'}' LIMIT 1;" \
    2>/dev/null | head -n 1
}

# Discover the exact Codex conversation for one AO workspace. A session_meta
# record is authoritative only when its first record names the exact workspace;
# the newest timestamp-qualified native id wins, while an exact timestamp tie
# is rejected. The function prints the selected id and every matching rollout
# path privately to its caller; no rollout contents or credentials are logged.
pr_green_find_native_rollouts() {
  local workspace="$1"
  local created_at="${2:-}" wanted_id="${3:-}" cutoff_epoch='' created_at_clean='' home file first metadata_id metadata_cwd metadata_timestamp metadata_epoch relative
  local best_id='' best_timestamp='' candidate_id candidate_timestamp best_ties=0
  local -a homes=()
  local -A seen_homes=() seen_ids=() id_timestamps=()
  local -a matches=()
  local candidates="${PR_GREEN_CODEX_HOME_CANDIDATES:-${HOME}/.codex:${CODEX_HOME:-${HOME}/.codex-dark-factory}}"

  if [[ -n "$created_at" ]]; then
    if [[ "$created_at" == *Z ]]; then
      cutoff_epoch="$(date -u -d "$created_at" +%s 2>/dev/null || true)"
    else
      created_at_clean="${created_at%%.*}"
      created_at_clean="${created_at_clean%% +*}"
      cutoff_epoch="$(date -u -d "$created_at_clean UTC" +%s 2>/dev/null || true)"
    fi
  fi
  IFS=: read -r -a homes <<<"$candidates"
  for home in "${homes[@]}"; do
    [[ -n "$home" && -d "$home/sessions" ]] || continue
    home="${home%/}"
    [[ -n "${seen_homes[$home]+yes}" ]] && continue
    seen_homes["$home"]=1
    while IFS= read -r -d '' file; do
      first="$(head -n 1 "$file" 2>/dev/null || true)"
      metadata_id="$(jq -r 'select(.type == "session_meta") | (.payload.session_id // .payload.id // empty)' <<<"$first" 2>/dev/null || true)"
      metadata_cwd="$(jq -r 'select(.type == "session_meta") | .payload.cwd // empty' <<<"$first" 2>/dev/null || true)"
      metadata_timestamp="$(jq -r 'select(.type == "session_meta") | .timestamp // .payload.timestamp // empty' <<<"$first" 2>/dev/null || true)"
      [[ -n "$metadata_id" && "$metadata_cwd" == "$workspace" ]] || continue
      [[ "$metadata_id" =~ ^[[:alnum:]-]{16,}$ ]] || continue
      [[ -z "$wanted_id" || "$metadata_id" == "$wanted_id" ]] || continue
      if [[ -n "$cutoff_epoch" ]]; then
        [[ -n "$metadata_timestamp" ]] || continue
        metadata_epoch="$(date -u -d "$metadata_timestamp" +%s 2>/dev/null || true)"
        [[ -n "$metadata_epoch" && "$metadata_epoch" -ge "$cutoff_epoch" ]] || continue
      fi
      seen_ids["$metadata_id"]=1
      if [[ -z "${id_timestamps[$metadata_id]+yes}" || "$metadata_timestamp" > "${id_timestamps[$metadata_id]}" ]]; then
        id_timestamps["$metadata_id"]="$metadata_timestamp"
      fi
      relative="${file#"$home/"}"
      matches+=("$metadata_id"$'\t'"$home"$'\t'"$relative")
    done < <(find "$home/sessions" -type f -name 'rollout-*.jsonl' -print0 2>/dev/null)
  done

  if [[ -n "$wanted_id" ]]; then
    [[ -n "${seen_ids[$wanted_id]+yes}" ]] || return 1
    best_id="$wanted_id"
  else
    for candidate_id in "${!seen_ids[@]}"; do
      candidate_timestamp="${id_timestamps[$candidate_id]}"
      if [[ -z "$best_id" || "$candidate_timestamp" > "$best_timestamp" ]]; then
        best_id="$candidate_id"
        best_timestamp="$candidate_timestamp"
        best_ties=0
      elif [[ "$candidate_timestamp" == "$best_timestamp" ]]; then
        best_ties=$((best_ties + 1))
      fi
    done
    ((best_ties == 0)) || return 1
  fi
  [[ -n "$best_id" ]] || return 1
  printf '%s\n' "$best_id"
  for match in "${matches[@]}"; do
    [[ "${match%%$'\t'*}" == "$best_id" ]] && printf '%s\n' "$match"
  done
}

# Copy all exact rollout segments for a unique native conversation into the
# authenticated project Codex home. Existing destination content is never
# overwritten: an identical file is accepted, while a differing file fails
# closed to protect conversation history.
pr_green_preserve_native_rollouts() {
  local intended_home="$1"
  local native_id="$2"
  local match source_home relative source target source_size target_size tmp rel candidate_size best_index best_source best_size i j
  local copied=0
  local -a sources=() relatives=()
  local -A seen_relatives=()
  while IFS=$'\t' read -r match source_home relative; do
    [[ "$match" == "$native_id" && -n "$source_home" && -n "$relative" ]] || continue
    source="$source_home/$relative"
    [[ -f "$source" && ! -L "$source" ]] || return 1
    sources+=("$source")
    relatives+=("$relative")
  done
  ((${#sources[@]} > 0)) || return 1

  # Several Codex homes can contain the same native id. Choose the longest
  # source only after proving every shorter copy is its exact byte prefix, so
  # an intended-profile continuation wins over an older shorter copy without
  # accepting divergent transcript branches.
  for i in "${!sources[@]}"; do
    rel="${relatives[$i]}"
    [[ -n "${seen_relatives[$rel]+yes}" ]] && continue
    seen_relatives["$rel"]=1
    best_index="$i"
    best_source="${sources[$i]}"
    best_size="$(wc -c <"$best_source")"
    for j in "${!sources[@]}"; do
      [[ "${relatives[$j]}" == "$rel" ]] || continue
      candidate_size="$(wc -c <"${sources[$j]}")"
      if [[ "$candidate_size" -gt "$best_size" ]]; then
        best_index="$j"
        best_source="${sources[$j]}"
        best_size="$candidate_size"
      fi
    done
    for j in "${!sources[@]}"; do
      [[ "${relatives[$j]}" == "$rel" ]] || continue
      candidate_size="$(wc -c <"${sources[$j]}")"
      cmp -n "$candidate_size" "$best_source" "${sources[$j]}" >/dev/null 2>&1 || return 1
    done

    target="$intended_home/$rel"
    mkdir -p "$(dirname "$target")" || return 1
    if [[ -e "$target" ]]; then
      [[ -f "$target" && ! -L "$target" ]] || return 1
      target_size="$(wc -c <"$target")"
      [[ "$target_size" -le "$best_size" ]] || return 1
      cmp -n "$target_size" "$best_source" "$target" >/dev/null 2>&1 || return 1
      if [[ "$target_size" -lt "$best_size" ]]; then
        tmp="${target}.tmp.$$"
        cp -p -- "$best_source" "$tmp" || { rm -f -- "$tmp"; return 1; }
        cmp -s "$best_source" "$tmp" || { rm -f -- "$tmp"; return 1; }
        mv -- "$tmp" "$target" || { rm -f -- "$tmp"; return 1; }
      fi
    else
      cp -p -- "$best_source" "$target" || return 1
      cmp -s "$best_source" "$target" || return 1
    fi
    copied=$((copied + 1))
  done
  ((copied > 0))
}

# Resolve the active AO daemon endpoint from the Go daemon's supported
# running.json handshake, with an explicit URL override reserved for isolated
# test/managed deployments. Do not use an unscoped status query or assume a
# fixed port.
pr_green_ao_api_base() {
  local override="${PR_GREEN_AO_API_URL:-}" run_file port
  if [[ -n "$override" ]]; then
    [[ "$override" =~ ^https?://[^[:space:]]+$ ]] || return 1
    printf '%s\n' "${override%/}"
    return 0
  fi
  run_file="${PR_GREEN_AO_RUN_FILE:-${AO_RUN_FILE:-$HOME/.ao/running.json}}"
  [[ -r "$run_file" ]] || return 1
  port="$(jq -r '.port // empty' <"$run_file" 2>/dev/null || true)"
  [[ "$port" =~ ^[0-9]+$ && "$port" -ge 1 && "$port" -le 65535 ]] || return 1
  printf 'http://127.0.0.1:%s\n' "$port"
}

pr_green_register_native_conversation() {
  local project_id="$1" session_id="$2" native_id="$3"
  local api_base body verified
  [[ "$native_id" =~ ^[[:alnum:]-]{16,}$ ]] || return 1
  api_base="$(pr_green_ao_api_base || true)"
  [[ -n "$api_base" ]] || return 1
  body="$(jq -cn --arg id "$native_id" '{agentSessionId:$id}')"
  curl --silent --show-error --fail-with-body --max-time "${PR_GREEN_AO_API_TIMEOUT_SECONDS:-10}" \
    -X POST "$api_base/api/v1/sessions/$session_id/activity" \
    -H 'Content-Type: application/json' --data-binary "$body" >/dev/null 2>/dev/null || return 1
  verified="$(pr_green_session_recovery_row "$project_id" "$session_id" | awk -F '|' '{print $2}' || true)"
  [[ "$verified" == "$native_id" ]]
}

# Recover a terminated Codex session's native conversation before AO restore.
# Return 0 when no recovery is needed or registration is verified; return 1
# for direct helper failures. The caller maps recovery failures to explicit
# recovery-blocked status (return 3), so an ambiguous history can never cause
# a fresh replacement session to be spawned.
pr_green_recover_native_conversation() {
  local project_id="$1" session_id="$2"
  local row workspace native_registered terminated intended_home native_id api_base body verified
  local -a native_rollouts=()
  local -a row_fields=()

  row="$(pr_green_session_recovery_row "$project_id" "$session_id" || true)"
  [[ -n "$row" ]] || return 1
  IFS='|' read -r -a row_fields <<<"$row"
  workspace="${row_fields[0]:-}"
  native_registered="${row_fields[1]:-}"
  terminated="${row_fields[2]:-}"
  local created_at="${row_fields[3]:-}"
  [[ -n "$workspace" && "$terminated" == "1" ]] || return 0
  intended_home="${CODEX_HOME:-${PR_GREEN_CODEX_HOME:-$HOME/.codex-dark-factory}}"
  [[ -d "$intended_home" && -s "$intended_home/auth.json" ]] || return 1
  if [[ -n "$native_registered" ]]; then
    mapfile -t native_rollouts < <(pr_green_find_native_rollouts "$workspace" '' "$native_registered") || return 1
    ((${#native_rollouts[@]} >= 2)) || return 1
    pr_green_preserve_native_rollouts "$intended_home" "$native_registered" < <(printf '%s\n' "${native_rollouts[@]:1}") || return 1
    return 0
  fi
  mapfile -t native_rollouts < <(pr_green_find_native_rollouts "$workspace" "$created_at") || return 1
  ((${#native_rollouts[@]} >= 2)) || return 1
  native_id="${native_rollouts[0]}"
  pr_green_preserve_native_rollouts "$intended_home" "$native_id" < <(printf '%s\n' "${native_rollouts[@]:1}") || return 1
  pr_green_register_native_conversation "$project_id" "$session_id" "$native_id"
}

# Find CODEX_HOME on the exact live Codex process under an AO tmux pane. The
# pane shell itself may not retain the exported variable, so inspect its child
# process tree without printing any other environment values.
pr_green_is_codex_process() {
  local pid="$1" exe command_line
  exe="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
  command_line="$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)"
  [[ "$exe" == */codex || "$exe" == */codex-* ]] \
    || [[ "$exe" == */node && "$command_line" == *codex.js* ]]
}

pr_green_process_codex_home() {
  local pid="$1" workspace="$2" home child child_home
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  [[ -r "/proc/$pid/environ" ]] || return 1
  pr_green_is_codex_process "$pid" || {
    command -v pgrep >/dev/null 2>&1 || return 1
    while IFS= read -r child; do
      [[ -n "$child" ]] || continue
      child_home="$(pr_green_process_codex_home "$child" "$workspace" || true)"
      if [[ -n "$child_home" ]]; then
        printf '%s\n' "$child_home"
        return 0
      fi
    done < <(pgrep -P "$pid" 2>/dev/null || true)
    return 1
  }
  home="$(tr '\0' '\n' <"/proc/$pid/environ" 2>/dev/null | awk -F= '$1 == "CODEX_HOME" {print $2; exit}')"
  if [[ -n "$home" && "$(readlink -f "/proc/$pid/cwd" 2>/dev/null || true)" == "$workspace" ]]; then
    printf '%s\n' "$home"
    return 0
  fi
  return 1
}

pr_green_live_codex_home() {
  local project_id="$1" session_id="$2" runtime_handle pane_pid
  runtime_handle="$(pr_green_runtime_handle "$project_id" "$session_id" || true)"
  [[ -n "$runtime_handle" ]] || return 1
  pane_pid="$(tmux list-panes -t "$runtime_handle" -F '#{pane_pid}' 2>/dev/null | head -n 1)"
  [[ "$pane_pid" =~ ^[0-9]+$ ]] || return 1
  local workspace="$3"
  pr_green_process_codex_home "$pane_pid" "$workspace"
}

# Register a live native identity only after proving the current Codex process
# uses the intended profile. Never copy its mutable rollout while it can append
# new events; an old-profile or uninspectable worker is deferred so no prompt
# or duplicate spawn is sent under an unverified account.
pr_green_preserve_live_native_conversation() {
  local project_id="$1" session_id="$2"
  local row workspace native_registered terminated intended_home created_at live_home native_id match source_home
  local -a row_fields=() native_rollouts=()
  row="$(pr_green_session_recovery_row "$project_id" "$session_id" || true)"
  # A public AO session may outlive a transient local DB read failure; leave
  # that live worker untouched and preserve ordinary reuse semantics.
  [[ -n "$row" ]] || return 0
  IFS='|' read -r -a row_fields <<<"$row"
  workspace="${row_fields[0]:-}"
  native_registered="${row_fields[1]:-}"
  terminated="${row_fields[2]:-}"
  created_at="${row_fields[3]:-}"
  [[ "$terminated" == "0" && -n "$workspace" ]] || return 0
  intended_home="${CODEX_HOME:-${PR_GREEN_CODEX_HOME:-$HOME/.codex-dark-factory}}"
  [[ -d "$intended_home" && -s "$intended_home/auth.json" ]] || return 1
  live_home="$(pr_green_live_codex_home "$project_id" "$session_id" "$workspace" || true)"
  [[ "$live_home" == "$intended_home" ]] || return 1
  if [[ -n "$native_registered" ]]; then
    # The native rollout remains in the process's own profile. It is mutable,
    # so identity proof is sufficient; transcript migration waits for AO
    # termination and the stable recovery path above.
    return 0
  fi
  mapfile -t native_rollouts < <(pr_green_find_native_rollouts "$workspace" "$created_at") || return 1
  ((${#native_rollouts[@]} >= 2)) || return 1
  native_id="${native_rollouts[0]}"
  source_home=''
  for match in "${native_rollouts[@]:1}"; do
    source_home="${match#*$'\t'}"
    source_home="${source_home%%$'\t'*}"
    [[ "$source_home" == "$intended_home" ]] && break
    source_home=''
  done
  [[ "$source_home" == "$intended_home" ]] || return 1
  pr_green_register_native_conversation "$project_id" "$session_id" "$native_id"
}

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
#   1 — no matching session; caller may spawn
#   2 — existing session accepted recovery/restore but did not accept the
#       prompt (including an ambiguous restore failure); caller must not spawn
#       a duplicate fleet member
#   3 — native recovery/identity could not be proven; caller must suppress
#       duplicate spawn without counting an inference attempt
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
    if ! pr_green_recover_native_conversation "$project_id" "$session_id"; then
      printf '%s\n' "AO terminated session $session_id has no unambiguous recoverable native conversation; suppressing duplicate spawn" >&2
      return 3
    fi
    if ! restore_output="$(ao session restore "$session_id" -p "$project_id" 2>&1)"; then
      printf '%s\n' "AO session $session_id could not be restored: $restore_output" >&2
      # Restore may have launched the native worker before its CLI observed an
      # error. Keep the exact existing session reserved until the next scan;
      # never let the caller create a duplicate from this ambiguous outcome.
      return 2
    fi
    printf '%s\n' "restored"
  else
    if pr_green_live_session_is_busy "$project_id" "$session_id"; then
      printf '%s\n' "busy_deferred"
      return 0
    fi
    if ! pr_green_preserve_live_native_conversation "$project_id" "$session_id"; then
      printf '%s\n' "AO live session $session_id has no safely migratable native rollout; suppressing duplicate spawn" >&2
      return 3
    fi
    printf '%s\n' "reused"
  fi

  if ! ao send --session "$session_id" --message "$prompt" >/dev/null 2>&1; then
    printf '%s\n' "AO session $session_id did not accept the prompt after reuse/restore; suppressing duplicate spawn" >&2
    return 2
  fi
}
