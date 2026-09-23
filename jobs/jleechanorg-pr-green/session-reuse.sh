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

# Query Codex's indexed thread registry before touching the session tree. A
# successful query is authoritative: an invalid or ambiguous row must not
# fall through to a filesystem "newest" guess. Return 2 when any candidate
# home lacks a queryable registry, allowing the legacy scan to handle mixed
# Codex homes and older profiles safely.
pr_green_find_native_rollouts_indexed() {
  local workspace="$1"
  local created_at="${2:-}" wanted_id="${3:-}"
  local cutoff_epoch='' created_at_clean='' home db escaped_workspace rows
  local candidate_id rollout_path first metadata_id metadata_cwd metadata_timestamp metadata_epoch
  local source_home relative match best_id='' best_timestamp='' candidate_timestamp filesystem_rollout filesystem_relative filesystem_key
  local best_ties=0
  local -a homes=() matches=()
  local -A seen_homes=() registry_queryable=() seen_ids=() id_timestamps=() seen_rollout_paths=()
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
  [[ "$workspace" != *$'\n'* && "$workspace" != *$'\r'* ]] || return 2
  escaped_workspace="$(printf '%s' "$workspace" | sed "s/'/''/g")"
  IFS=: read -r -a homes <<<"$candidates"
  for home in "${homes[@]}"; do
    [[ -n "$home" ]] || continue
    home="${home%/}"
    [[ -n "${seen_homes[$home]+yes}" ]] && continue
    seen_homes["$home"]=1
    registry_queryable["$home"]=0
    for db in "$home/state_5.sqlite" "$home/state.sqlite"; do
      [[ -r "$db" ]] || continue
      if ! rows="$(sqlite3 -readonly -noheader -separator $'\t' "$db" "SELECT id,rollout_path,updated_at,created_at FROM threads WHERE cwd = '$escaped_workspace' ORDER BY updated_at DESC;" 2>/dev/null)"; then
        continue
      fi
      registry_queryable["$home"]=1
      while IFS=$'\t' read -r candidate_id rollout_path _ _; do
        [[ -n "$candidate_id" && -n "$rollout_path" ]] || continue
        [[ -n "$wanted_id" && "$candidate_id" != "$wanted_id" ]] && continue
        [[ -n "$wanted_id" || "$candidate_id" =~ ^[[:alnum:]-]{16,}$ ]] || continue
        case "$rollout_path" in
          /*) ;;
          *) rollout_path="$home/$rollout_path" ;;
        esac
        [[ -f "$rollout_path" && ! -L "$rollout_path" ]] || continue
        first="$(head -n 1 "$rollout_path" 2>/dev/null || true)"
        metadata_id="$(jq -r 'select(.type == "session_meta") | (.payload.session_id // .payload.id // empty)' <<<"$first" 2>/dev/null || true)"
        metadata_cwd="$(jq -r 'select(.type == "session_meta") | .payload.cwd // empty' <<<"$first" 2>/dev/null || true)"
        metadata_timestamp="$(jq -r 'select(.type == "session_meta") | .timestamp // .payload.timestamp // empty' <<<"$first" 2>/dev/null || true)"
        [[ "$metadata_id" == "$candidate_id" && "$metadata_cwd" == "$workspace" ]] || continue
        if [[ -n "$cutoff_epoch" ]]; then
          [[ -n "$metadata_timestamp" ]] || continue
          metadata_epoch="$(date -u -d "$metadata_timestamp" +%s 2>/dev/null || true)"
          [[ -n "$metadata_epoch" && "$metadata_epoch" -ge "$cutoff_epoch" ]] || continue
        fi
        seen_ids["$candidate_id"]=1
        if [[ -z "${id_timestamps[$candidate_id]+yes}" || "$metadata_timestamp" > "${id_timestamps[$candidate_id]}" ]]; then
          id_timestamps["$candidate_id"]="$metadata_timestamp"
        fi
        case "$rollout_path" in
          "$home"/*) source_home="$home"; relative="${rollout_path#$home/}" ;;
          /*) source_home=/; relative="${rollout_path#/}" ;;
          *) continue ;;
        esac
        seen_rollout_paths["$source_home/$relative"]=1
        matches+=("$candidate_id"$'\t'"$source_home"$'\t'"$relative")
      done <<<"$rows"
    done
  done
  # A restored copy can exist in a profile whose thread index has not yet
  # received metadata. For an exact requested id, include that filesystem
  # copy alongside indexed rows instead of returning only the source profile.
  if [[ -n "$wanted_id" ]]; then
    for home in "${homes[@]}"; do
      home="${home%/}"
      [[ -d "$home/sessions" ]] || continue
      while IFS= read -r -d '' filesystem_rollout; do
        [[ -f "$filesystem_rollout" && ! -L "$filesystem_rollout" ]] || continue
        first="$(head -n 1 "$filesystem_rollout" 2>/dev/null || true)"
        metadata_id="$(jq -r 'select(.type == "session_meta") | (.payload.session_id // .payload.id // empty)' <<<"$first" 2>/dev/null || true)"
        metadata_cwd="$(jq -r 'select(.type == "session_meta") | .payload.cwd // empty' <<<"$first" 2>/dev/null || true)"
        metadata_timestamp="$(jq -r 'select(.type == "session_meta") | .timestamp // .payload.timestamp // empty' <<<"$first" 2>/dev/null || true)"
        [[ "$metadata_id" == "$wanted_id" && "$metadata_cwd" == "$workspace" ]] || continue
        if [[ -n "$cutoff_epoch" ]]; then
          [[ -n "$metadata_timestamp" ]] || continue
          metadata_epoch="$(date -u -d "$metadata_timestamp" +%s 2>/dev/null || true)"
          [[ -n "$metadata_epoch" && "$metadata_epoch" -ge "$cutoff_epoch" ]] || continue
        fi
        filesystem_relative="${filesystem_rollout#"$home/"}"
        filesystem_key="$home/$filesystem_relative"
        [[ -n "${seen_rollout_paths[$filesystem_key]+yes}" ]] && continue
        seen_rollout_paths["$filesystem_key"]=1
        seen_ids["$wanted_id"]=1
        if [[ -z "${id_timestamps[$wanted_id]+yes}" || "$metadata_timestamp" > "${id_timestamps[$wanted_id]}" ]]; then
          id_timestamps["$wanted_id"]="$metadata_timestamp"
        fi
        matches+=("$wanted_id"$'\t'"$home"$'\t'"$filesystem_relative")
      done < <(find "$home/sessions" -type f -name "*${wanted_id}*.jsonl" -print0 2>/dev/null)
    done
  fi
  ((${#seen_homes[@]} > 0)) || return 2
  for home in "${!seen_homes[@]}"; do
    ((registry_queryable[$home] == 1)) || return 2
  done
  if [[ -n "$wanted_id" ]]; then
    [[ -n "${seen_ids[$wanted_id]+yes}" ]] || return 1
    best_id="$wanted_id"
  else
    for candidate_id in "${!seen_ids[@]}"; do
      candidate_timestamp="${id_timestamps[$candidate_id]}"
      [[ -n "$candidate_timestamp" ]] || continue
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
  return 0
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
  local indexed_result indexed_rc=0
  local -a homes=()
  local -A seen_homes=() seen_ids=() id_timestamps=()
  local -a matches=()
  local candidates="${PR_GREEN_CODEX_HOME_CANDIDATES:-${HOME}/.codex:${CODEX_HOME:-${HOME}/.codex-dark-factory}}"

  indexed_result="$(pr_green_find_native_rollouts_indexed "$workspace" "$created_at" "$wanted_id" 2>/dev/null)" || indexed_rc=$?
  if ((indexed_rc == 0 || indexed_rc == 1)); then
    [[ -n "$indexed_result" ]] && printf '%s\n' "$indexed_result"
    return "$indexed_rc"
  fi

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
    if [[ -n "$wanted_id" ]]; then
      while IFS= read -r -d '' file; do
        first="$(head -n 1 "$file" 2>/dev/null || true)"
        metadata_id="$(jq -r 'select(.type == "session_meta") | (.payload.session_id // .payload.id // empty)' <<<"$first" 2>/dev/null || true)"
        metadata_cwd="$(jq -r 'select(.type == "session_meta") | .payload.cwd // empty' <<<"$first" 2>/dev/null || true)"
        metadata_timestamp="$(jq -r 'select(.type == "session_meta") | .timestamp // .payload.timestamp // empty' <<<"$first" 2>/dev/null || true)"
        [[ "$metadata_id" == "$wanted_id" && "$metadata_cwd" == "$workspace" ]] || continue
        seen_ids["$metadata_id"]=1
        id_timestamps["$metadata_id"]="$metadata_timestamp"
        relative="${file#"$home/"}"
        matches+=("$metadata_id"$'\t'"$home"$'\t'"$relative")
      done < <(find "$home/sessions" -type f -name "*${wanted_id}*.jsonl" -print0 2>/dev/null)
      continue
    fi
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
  return 0
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
  local native_listing
  native_listing="$(pr_green_find_native_rollouts "$workspace" "$created_at" 2>/dev/null)" || return 1
  mapfile -t native_rollouts <<<"$native_listing"
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
  PR_GREEN_NATIVE_ROLLOUT_LISTING="$native_listing"
  pr_green_register_native_conversation "$project_id" "$session_id" "$native_id"
}

# Count native Codex user-turns whose exact submitted prompt body is present for
# one exact workspace/conversation. This is a transport acknowledgement, not
# semantic inspection: a successful AO HTTP response is insufficient until the
# native rollout records the requested body.
pr_green_native_prompt_count() {
  local listing="$1" prompt="$2" line home relative file count total=0
  local -a entries=()
  mapfile -t entries <<<"$listing"
  ((${#entries[@]} >= 2)) || return 1
  for line in "${entries[@]:1}"; do
    [[ -n "$line" ]] || continue
    home="${line#*$'\t'}"
    relative="${home#*$'\t'}"
    home="${home%%$'\t'*}"
    file="$home/$relative"
    [[ -f "$file" && ! -L "$file" ]] || return 1
    count="$(jq -s --arg prompt "$prompt" '[.[] | select(.type == "response_item" and .payload.role == "user") | ([.payload.content[]? | select(.type == "input_text") | .text] | join("")) | select(. == $prompt)] | length' "$file" 2>/dev/null)" || return 1
    total=$((total + count))
  done
  printf '%s\n' "$total"
}

# Count delivery envelopes in native USER response items. Unlike the ordinary
# prompt counter above, this is deliberately scoped to persisted delivery
# state: AO may prepend its raw CI request, but the complete known envelope
# must still occur in the USER body. A nonce alone is never sufficient.
pr_green_delivery_ack_count() {
  local listing="$1" envelope="$2" legacy_envelope="${3:-}" line home relative file count total=0
  local -a entries=()
  [[ -n "$envelope" ]] || return 1
  mapfile -t entries <<<"$listing"
  ((${#entries[@]} >= 2)) || return 1
  for line in "${entries[@]:1}"; do
    [[ -n "$line" ]] || continue
    home="${line#*$'\t'}"
    relative="${home#*$'\t'}"
    home="${home%%$'\t'*}"
    file="$home/$relative"
    [[ -f "$file" && ! -L "$file" ]] || return 1
    count="$(jq -s --arg envelope "$envelope" --arg legacy_envelope "$legacy_envelope" '
      [.[]
       | select(.type == "response_item" and .payload.role == "user")
       | ([.payload.content[]? | select(.type == "input_text") | .text] | join("")) as $body
       | select(($body | contains($envelope)) or ($legacy_envelope != "" and ($body | contains($legacy_envelope))))]
      | length
    ' "$file" 2>/dev/null)" || return 1
    total=$((total + count))
  done
  printf '%s\n' "$total"
}

pr_green_wait_for_native_prompt() {
  local listing="$1" prompt="$2" baseline="$3"
  local timeout_seconds="${4:-${PR_GREEN_NATIVE_ACK_TIMEOUT_SECONDS:-8}}"
  local current deadline
  [[ "$baseline" =~ ^[0-9]+$ && "$timeout_seconds" =~ ^[0-9]+$ ]] || return 1
  deadline=$((SECONDS + timeout_seconds))
  while :; do
    current="$(pr_green_native_prompt_count "$listing" "$prompt" 2>/dev/null || true)"
    [[ "$current" =~ ^[0-9]+$ && "$current" -gt "$baseline" ]] && return 0
    (( SECONDS >= deadline )) && return 1
    sleep 0.25
  done
}

pr_green_wait_for_delivery_ack() {
  local listing="$1" envelope="$2" legacy_envelope="$3" baseline="$4"
  local timeout_seconds="${5:-${PR_GREEN_NATIVE_ACK_TIMEOUT_SECONDS:-8}}"
  local current deadline
  [[ "$baseline" =~ ^[0-9]+$ && "$timeout_seconds" =~ ^[0-9]+$ ]] || return 1
  deadline=$((SECONDS + timeout_seconds))
  while :; do
    current="$(pr_green_delivery_ack_count "$listing" "$envelope" "$legacy_envelope" 2>/dev/null || true)"
    [[ "$current" =~ ^[0-9]+$ && "$current" -gt "$baseline" ]] && return 0
    (( SECONDS >= deadline )) && return 1
    sleep 0.25
  done
}

# A successful AO transport call is not durable delivery. Persist the exact
# envelope before sending it so a later sweep can prove (or suppress) the same
# request instead of appending another prompt to the native composer.
pr_green_delivery_state_dir() {
  printf '%s\n' "${PR_GREEN_DELIVERY_STATE_DIR:-${PR_GREEN_METRICS_DIR:-${TMPDIR:-/tmp}/jleechanorg-pr-green}/pending-delivery}"
}

pr_green_delivery_pending_path() {
  local project_id="$1" pr_number="$2" state_dir
  [[ "$project_id" =~ ^[[:alnum:]._-]+$ && "$pr_number" =~ ^[0-9]+$ ]] || return 1
  state_dir="$(pr_green_delivery_state_dir)"
  printf '%s/%s-%s.json\n' "$state_dir" "$project_id" "$pr_number"
}

pr_green_delivery_envelope() {
  local brief_path="$1" request_id="$2"
  printf 'Read and execute %s. [PR_GREEN_DELIVERY_ID:%s]\n' "$brief_path" "$request_id"
}

pr_green_delivery_request_id() {
  if [[ -n "${PR_GREEN_TEST_DELIVERY_ID:-}" ]]; then
    printf '%s\n' "$PR_GREEN_TEST_DELIVERY_ID"
  else
    printf 'pr-green-%s-%s-%s\n' "$(date +%s%N)" "$$" "${RANDOM:-0}"
  fi
}

pr_green_delivery_write_brief() {
  local project_id="$1" pr_number="$2" request_id="$3" prompt="$4"
  local pending_path brief_path state_dir tmp
  [[ "$request_id" =~ ^[[:alnum:]_.-]+$ ]] || return 1
  pending_path="$(pr_green_delivery_pending_path "$project_id" "$pr_number")" || return 1
  state_dir="$(dirname "$pending_path")"
  mkdir -p "$state_dir" || return 1
  brief_path="${pending_path%.json}.${request_id}.brief"
  if [[ -e "$brief_path" ]]; then
    # A prior crash may have committed the immutable brief before the ledger.
    # Reuse it only when it is the exact regular, mode-600 content; never
    # overwrite a divergent brief for the same request id.
    [[ -f "$brief_path" && ! -L "$brief_path" ]] || return 1
    [[ "$(stat -c '%a' "$brief_path" 2>/dev/null || true)" == 600 ]] || return 1
    cmp -s <(printf '%s' "$prompt") "$brief_path" || return 1
    printf '%s\n' "$brief_path"
    return 0
  fi
  tmp="$(mktemp "${brief_path}.tmp.XXXXXX")" || return 1
  if ! printf '%s' "$prompt" >"$tmp"; then
    rm -f -- "$tmp"
    return 1
  fi
  chmod 600 -- "$tmp" || { rm -f -- "$tmp"; return 1; }
  mv -- "$tmp" "$brief_path" || { rm -f -- "$tmp"; return 1; }
  printf '%s\n' "$brief_path"
}

pr_green_delivery_write_pending() {
  local project_id="$1" pr_number="$2" session_id="$3" native_id="$4" workspace="$5" prompt="$6" envelope="$7" request_id="$8" brief_path="$9"
  local path state_dir tmp
  path="$(pr_green_delivery_pending_path "$project_id" "$pr_number")" || return 1
  state_dir="$(dirname "$path")"
  mkdir -p "$state_dir" || return 1
  tmp="$(mktemp "${path}.tmp.XXXXXX")" || return 1
  if ! jq -cn --arg project_id "$project_id" --argjson pr_number "$pr_number" \
    --arg session_id "$session_id" --arg native_id "$native_id" --arg workspace "$workspace" \
    --arg prompt "$prompt" --arg envelope "$envelope" --arg request_id "$request_id" --arg brief_path "$brief_path" \
    --argjson created_at "$(date +%s)" \
    '{project_id:$project_id,pr_number:$pr_number,session_id:$session_id,native_id:$native_id,workspace:$workspace,prompt:$prompt,envelope:$envelope,brief_path:$brief_path,request_id:$request_id,created_at:$created_at}' \
    >"$tmp"; then
    rm -f -- "$tmp"
    return 1
  fi
  mv -- "$tmp" "$path"
}

pr_green_delivery_clear_pending() {
  local path
  path="$(pr_green_delivery_pending_path "$1" "$2")" || return 1
  rm -f -- "$path"
}

# Return acked when the persisted request's exact native envelope exists,
# pending when it does not, and none when no request is outstanding. A stale
# or malformed record is deliberately pending: it must suppress a duplicate,
# never become permission to send a new prompt.
pr_green_delivery_pending_status() {
  local project_id="$1" pr_number="$2" path pending listing count envelope legacy_envelope native_id workspace
  path="$(pr_green_delivery_pending_path "$project_id" "$pr_number")" || return 1
  [[ -s "$path" ]] || { printf '%s\n' none; return 0; }
  pending="$(cat -- "$path" 2>/dev/null || true)"
  native_id="$(jq -r '.native_id // empty' <<<"$pending" 2>/dev/null || true)"
  workspace="$(jq -r '.workspace // empty' <<<"$pending" 2>/dev/null || true)"
  envelope="$(jq -r '.envelope // empty' <<<"$pending" 2>/dev/null || true)"
  legacy_envelope="$(jq -r '.legacy_envelope // empty' <<<"$pending" 2>/dev/null || true)"
  [[ -n "$native_id" && -n "$workspace" && -n "$envelope" ]] || { printf '%s\n' pending; return 0; }
  listing="$(pr_green_find_native_rollouts "$workspace" '' "$native_id" 2>/dev/null || true)"
  [[ -n "$listing" ]] || { printf '%s\n' pending; return 0; }
  count="$(pr_green_delivery_ack_count "$listing" "$envelope" "$legacy_envelope" 2>/dev/null || true)"
  if [[ "$count" =~ ^[0-9]+$ && "$count" -gt 0 ]]; then
    printf '%s\n' acked
  else
    printf '%s\n' pending
  fi
}

# Upgrade a pre-pointer ledger entry without appending a prompt.  The old
# envelope is checked first so a delayed native acknowledgement retires the
# old request rather than creating a second one.  A non-acknowledged legacy
# entry is converted to the immutable-brief form, but the caller may deliver
# that pointer only after a terminated exact session has been restored; live
# sessions stay fail-closed until their existing composer is known safe.
pr_green_delivery_upgrade_legacy() {
  local project_id="$1" pr_number="$2" listing="$3" native_id="$4" workspace="$5"
  local path pending old_envelope prompt request_id brief_path envelope count tmp legacy_pending
  path="$(pr_green_delivery_pending_path "$project_id" "$pr_number")" || return 1
  [[ -s "$path" ]] || { printf '%s\n' none; return 0; }
  pending="$(cat -- "$path" 2>/dev/null || true)"
  brief_path="$(jq -r '.brief_path // empty' <<<"$pending" 2>/dev/null || true)"
  legacy_pending="$(jq -r '.legacy_pending // false' <<<"$pending" 2>/dev/null || true)"
  [[ -n "$brief_path" && "$legacy_pending" != true ]] && { printf '%s\n' current; return 0; }
  [[ -n "$brief_path" && "$legacy_pending" == true ]] && { printf '%s\n' migrated; return 0; }
  old_envelope="$(jq -r '.envelope // empty' <<<"$pending" 2>/dev/null || true)"
  prompt="$(jq -r '.prompt // empty' <<<"$pending" 2>/dev/null || true)"
  request_id="$(jq -r '.request_id // empty' <<<"$pending" 2>/dev/null || true)"
  [[ -n "$old_envelope" && -n "$prompt" && "$native_id" == "$(jq -r '.native_id // empty' <<<"$pending" 2>/dev/null || true)" ]] || {
    printf '%s\n' pending
    return 0
  }
  [[ "$workspace" == "$(jq -r '.workspace // empty' <<<"$pending" 2>/dev/null || true)" ]] || {
    printf '%s\n' pending
    return 0
  }
  count="$(pr_green_delivery_ack_count "$listing" "$old_envelope" 2>/dev/null || true)"
  if [[ "$count" =~ ^[0-9]+$ && "$count" -gt 0 ]]; then
    printf '%s\n' acked
    return 0
  fi
  [[ "$request_id" =~ ^[[:alnum:]_.-]+$ ]] || { printf '%s\n' pending; return 0; }
  brief_path="$(pr_green_delivery_write_brief "$project_id" "$pr_number" "$request_id" "$prompt" 2>/dev/null || true)"
  [[ -n "$brief_path" ]] || { printf '%s\n' pending; return 0; }
  envelope="$(pr_green_delivery_envelope "$brief_path" "$request_id")"
  tmp="$(mktemp "${path}.tmp.XXXXXX")" || { printf '%s\n' pending; return 0; }
  if ! jq --arg envelope "$envelope" --arg brief_path "$brief_path" --arg legacy_envelope "$old_envelope" \
    '. + {envelope:$envelope,brief_path:$brief_path,legacy_envelope:$legacy_envelope,transport:"pointer-v1",legacy_pending:true}' \
    <<<"$pending" >"$tmp"; then
    rm -f -- "$tmp"
    printf '%s\n' pending
    return 0
  fi
  mv -- "$tmp" "$path" || { rm -f -- "$tmp"; printf '%s\n' pending; return 0; }
  printf '%s\n' migrated
}

pr_green_delivery_recovery_composer_region() {
  awk '
    /^[[:space:]]*›[[:space:]]/ {
      found=1
      region=$0
      sub(/^[[:space:]]*›[[:space:]]*/, "", region)
      next
    }
    found && $0 ~ /^[[:space:]]*(\? for shortcuts|GPT-[0-9.]+-[[:alnum:]-]+[[:space:]]+high[[:space:]]+·[[:space:]]+~\/\.ao\/data\/worktrees\/[^·]+·[[:space:]]+Repair[[:space:]]+PR[[:space:]]+#[0-9]+[[:space:]]+tests[[:space:]]+⚠[[:space:]]*[0-9]+[[:space:]]+warnings?[[:space:]]*·[[:space:]]*f2[[:space:]]+to[[:space:]]+view)/ { exit }
    found { region=region "\n" $0 }
    END { if (found) print region }
  '
}

pr_green_delivery_recovery_normalize() {
  # TUI wrapping can split even an otherwise single token; remove only
  # whitespace from both sides, preserving every non-whitespace byte.
  tr -d '[:space:]'
}

pr_green_delivery_recovery_capture() {
  local pane_target="$1" shape mode cursor_y pane_height pane footer_line
  shape="$(tmux display-message -p -t "$pane_target" '#{pane_in_mode}:#{cursor_y}:#{pane_height}' 2>/dev/null || true)"
  IFS=: read -r mode cursor_y pane_height <<<"$shape"
  [[ "$mode" == 0 && "$cursor_y" =~ ^[0-9]+$ && "$pane_height" =~ ^[0-9]+$ && "$cursor_y" -lt "$pane_height" ]] || return 1
  pane="$(tmux capture-pane -p -t "$pane_target" -S 0 -E "$((pane_height - 1))" 2>/dev/null || true)"
  footer_line="$(grep -En '^[[:space:]]*(\? for shortcuts|GPT-[0-9.]+-[[:alnum:]-]+[[:space:]]+high[[:space:]]+·[[:space:]]+~\/\.ao\/data\/worktrees\/[^·]+·[[:space:]]+Repair[[:space:]]+PR[[:space:]]+#[0-9]+[[:space:]]+tests[[:space:]]+⚠[[:space:]]*[0-9]+[[:space:]]+warnings?[[:space:]]*·[[:space:]]*f2[[:space:]]+to[[:space:]]+view)' <<<"$pane" | head -n 1 | cut -d: -f1 || true)"
  [[ "$footer_line" =~ ^[0-9]+$ && "$footer_line" -gt "$((cursor_y + 1))" ]] || return 1
  printf '%s\n' "$pane"
}

pr_green_delivery_recovery_composer_matches() {
  local region="$1" envelope="$2" legacy_envelope="${3:-}" normalized expected
  normalized="$(pr_green_delivery_recovery_normalize <<<"$region")"
  [[ -n "$normalized" ]] || return 1
  for expected in "$envelope" "$legacy_envelope"; do
    [[ -n "$expected" ]] || continue
    expected="$(pr_green_delivery_recovery_normalize <<<"$expected")"
    [[ -n "$expected" && "$normalized" == *"$expected" ]] && return 0
  done
  return 1
}

pr_green_delivery_recovery_identity() {
  local project_id="$1" pr_number="$2" session_id="$3" native_id="$4" workspace="$5" expected_runtime="${6:-}" expected_pane_id="${7:-}"
  local record fresh_session row row_workspace row_native row_terminated live_home intended_home runtime_handle pane_id pane_ids
  local -a row_fields=()
  record="$(pr_green_session_record "$project_id" "$pr_number" 2>/dev/null || true)"
  fresh_session="$(jq -r '.id // empty' <<<"$record" 2>/dev/null || true)"
  [[ -n "$record" && "$fresh_session" == "$session_id" ]] || return 1
  [[ "$(jq -r 'if (.isTerminated // false) then "true" else "false" end' <<<"$record" 2>/dev/null || true)" == false ]] || return 1
  row="$(pr_green_session_recovery_row "$project_id" "$session_id" 2>/dev/null || true)"
  IFS='|' read -r -a row_fields <<<"$row"
  row_workspace="${row_fields[0]:-}"
  row_native="${row_fields[1]:-}"
  row_terminated="${row_fields[2]:-}"
  [[ "$row_workspace" == "$workspace" && "$row_native" == "$native_id" && "$row_terminated" == 0 ]] || return 1
  intended_home="${CODEX_HOME:-${PR_GREEN_CODEX_HOME:-$HOME/.codex-dark-factory}}"
  [[ -d "$intended_home" && -s "$intended_home/auth.json" ]] || return 1
  live_home="$(pr_green_live_codex_home "$project_id" "$session_id" "$workspace" 2>/dev/null || true)"
  [[ "$live_home" == "$intended_home" ]] || return 1
  pr_green_live_session_is_busy "$project_id" "$session_id" && return 2
  runtime_handle="$(pr_green_runtime_handle "$project_id" "$session_id" 2>/dev/null || true)"
  [[ -n "$runtime_handle" && ( -z "$expected_runtime" || "$runtime_handle" == "$expected_runtime" ) ]] || return 1
  pane_ids="$(tmux list-panes -t "$runtime_handle" -F '#{pane_id}' 2>/dev/null || true)"
  [[ -n "$pane_ids" && "$pane_ids" != *$'\n'* ]] || return 1
  pane_id="$pane_ids"
  [[ "$pane_id" =~ ^%[0-9]+$ && ( -z "$expected_pane_id" || "$pane_id" == "$expected_pane_id" ) ]] || return 1
  PR_GREEN_DELIVERY_RECOVERY_RUNTIME="$runtime_handle"
  PR_GREEN_DELIVERY_RECOVERY_PANE_ID="$pane_id"
}

pr_green_delivery_mark_recovery_attempted() {
  local path="$1" evidence_path="$2" runtime_handle="$3" pane_id="$4" envelope_digest="$5" tmp attempted_at
  attempted_at="$(date +%s)"
  [[ "$attempted_at" =~ ^[0-9]+$ ]] || return 1
  tmp="$(mktemp "${path}.tmp.XXXXXX")" || return 1
  if ! jq --argjson attempted_at "$attempted_at" --arg evidence_path "$evidence_path" --arg runtime_handle "$runtime_handle" \
    --arg pane_id "$pane_id" --arg envelope_digest "$envelope_digest" \
    '. + {submit_recovery_attempted_at:$attempted_at,submit_recovery_evidence_path:$evidence_path,submit_recovery_runtime_handle:$runtime_handle,submit_recovery_pane_id:$pane_id,submit_recovery_envelope_sha256:$envelope_digest}' \
    <"$path" >"$tmp"; then
    rm -f -- "$tmp"
    return 1
  fi
  mv -- "$tmp" "$path" || { rm -f -- "$tmp"; return 1; }
}

# Submit only the already-visible pending composer draft. This never appends
# text, interrupts, kills, restarts, or replays an AO message.
pr_green_delivery_submit_pending_recovery() {
  local project_id="$1" pr_number="$2" path pending session_id native_id workspace envelope legacy_envelope request_id
  local listing baseline runtime_handle pane region normalized second_pane second_region second_normalized pending_guard refreshed_pending
  local state_dir evidence_path tmp claim_path marker_count current_ack pane_id refreshed_listing envelope_digest
  path="$(pr_green_delivery_pending_path "$project_id" "$pr_number" 2>/dev/null || true)"
  [[ -s "$path" ]] || return 1
  pending="$(cat -- "$path" 2>/dev/null || true)"
  session_id="$(jq -r '.session_id // empty' <<<"$pending" 2>/dev/null || true)"
  native_id="$(jq -r '.native_id // empty' <<<"$pending" 2>/dev/null || true)"
  workspace="$(jq -r '.workspace // empty' <<<"$pending" 2>/dev/null || true)"
  envelope="$(jq -r '.envelope // empty' <<<"$pending" 2>/dev/null || true)"
  legacy_envelope="$(jq -r '.legacy_envelope // empty' <<<"$pending" 2>/dev/null || true)"
  request_id="$(jq -r '.request_id // empty' <<<"$pending" 2>/dev/null || true)"
  [[ "$session_id" =~ ^[[:alnum:]._-]+$ && "$native_id" =~ ^[[:alnum:]-]{16,}$ && -n "$workspace" && -n "$envelope" ]] || return 1
  [[ "$request_id" =~ ^[[:alnum:]_.-]+$ && "$envelope" == *"[PR_GREEN_DELIVERY_ID:$request_id]"* ]] || return 1
  jq -e '(.submit_recovery_attempted_at // null) | numbers' <<<"$pending" >/dev/null 2>&1 && return 1
  pending_guard="$(jq -c . <<<"$pending" 2>/dev/null || true)"
  [[ -n "$pending_guard" ]] || return 1
  state_dir="$(dirname -- "$path")"
  evidence_path="$state_dir/.${project_id}-${pr_number}-${request_id}.submit-recovery-pane"
  claim_path="${evidence_path}.lock"
  mkdir -- "$claim_path" 2>/dev/null || return 1
  refreshed_pending="$(cat -- "$path" 2>/dev/null || true)"
  [[ "$(jq -c . <<<"$refreshed_pending" 2>/dev/null || true)" == "$pending_guard" ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  if ! pr_green_delivery_recovery_identity "$project_id" "$pr_number" "$session_id" "$native_id" "$workspace"; then
    rmdir -- "$claim_path" 2>/dev/null || true
    return 1
  fi
  runtime_handle="$PR_GREEN_DELIVERY_RECOVERY_RUNTIME"
  pane_id="$PR_GREEN_DELIVERY_RECOVERY_PANE_ID"
  listing="$(pr_green_find_native_rollouts "$workspace" '' "$native_id" 2>/dev/null || true)"
  [[ -n "$listing" ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  baseline="$(pr_green_delivery_ack_count "$listing" "$envelope" "$legacy_envelope" 2>/dev/null || true)"
  [[ "$baseline" =~ ^[0-9]+$ && "$baseline" == 0 ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }

  pane="$(pr_green_delivery_recovery_capture "$pane_id" 2>/dev/null || true)"
  [[ -n "$pane" ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  marker_count="$(grep -Ec '^[[:space:]]*›[[:space:]]' <<<"$pane" || true)"
  [[ "$marker_count" == 1 ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  region="$(pr_green_delivery_recovery_composer_region <<<"$pane")"
  pr_green_delivery_recovery_composer_matches "$region" "$envelope" "$legacy_envelope" || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  normalized="$(pr_green_delivery_recovery_normalize <<<"$region")"
  tmp="$(mktemp "${evidence_path}.XXXXXX")" || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  if ! printf '%s\n' "$pane" >"$tmp" || ! chmod 600 -- "$tmp"; then
    rm -f -- "$tmp"
    rmdir -- "$claim_path" 2>/dev/null || true
    return 1
  fi
  mv -- "$tmp" "$evidence_path" || { rm -f -- "$tmp"; rmdir -- "$claim_path" 2>/dev/null || true; return 1; }

  # Re-read every identity and the composer immediately before the one Enter.
  pending="$(cat -- "$path" 2>/dev/null || true)"
  [[ "$(jq -c . <<<"$pending" 2>/dev/null || true)" == "$pending_guard" ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  jq -e '(.submit_recovery_attempted_at // null) | numbers' <<<"$pending" >/dev/null 2>&1 && { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  if ! pr_green_delivery_recovery_identity "$project_id" "$pr_number" "$session_id" "$native_id" "$workspace" "$runtime_handle" "$pane_id"; then
    rmdir -- "$claim_path" 2>/dev/null || true
    return 1
  fi
  [[ "$PR_GREEN_DELIVERY_RECOVERY_PANE_ID" == "$pane_id" ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  second_pane="$(pr_green_delivery_recovery_capture "$pane_id" 2>/dev/null || true)"
  [[ -n "$second_pane" ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  marker_count="$(grep -Ec '^[[:space:]]*›[[:space:]]' <<<"$second_pane" || true)"
  if [[ "$marker_count" != 1 ]]; then
    rmdir -- "$claim_path" 2>/dev/null || true
    return 1
  fi
  second_region="$(pr_green_delivery_recovery_composer_region <<<"$second_pane")"
  second_normalized="$(pr_green_delivery_recovery_normalize <<<"$second_region")"
  if [[ "$second_normalized" != "$normalized" ]] || ! pr_green_delivery_recovery_composer_matches "$second_region" "$envelope" "$legacy_envelope"; then
    rmdir -- "$claim_path" 2>/dev/null || true
    return 1
  fi
  refreshed_listing="$(pr_green_find_native_rollouts "$workspace" '' "$native_id" 2>/dev/null || true)"
  [[ "${refreshed_listing%%$'\n'*}" == "$native_id" ]] || { rmdir -- "$claim_path" 2>/dev/null || true; return 1; }
  current_ack="$(pr_green_delivery_ack_count "$refreshed_listing" "$envelope" "$legacy_envelope" 2>/dev/null || true)"
  if [[ ! "$current_ack" =~ ^[0-9]+$ ]]; then
    rmdir -- "$claim_path" 2>/dev/null || true
    return 1
  fi
  if [[ "$current_ack" =~ ^[0-9]+$ && "$current_ack" -gt "$baseline" ]]; then
    pr_green_delivery_clear_pending "$project_id" "$pr_number"
    return $?
  fi
  envelope_digest="$(printf '%s' "$envelope" | sha256sum | awk '{print $1}')"
  if ! pr_green_delivery_mark_recovery_attempted "$path" "$evidence_path" "$runtime_handle" "$pane_id" "$envelope_digest"; then
    rmdir -- "$claim_path" 2>/dev/null || true
    return 1
  fi
  tmux send-keys -t "$pane_id" Enter || return 1
  if pr_green_wait_for_delivery_ack "$refreshed_listing" "$envelope" "$legacy_envelope" "$baseline"; then
    pr_green_delivery_clear_pending "$project_id" "$pr_number"
    return $?
  fi
  return 2
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
#   4 — AO accepted transport, but no native user turn was observed and no
#       safe replay path was proven; caller must persist delivery_unconfirmed
#       without sending a duplicate
pr_green_reuse_session() {
  local project_id="$1"
  local pr_number="$2"
  local prompt="$3"
  local record session_id terminated restore_output native_row native_workspace native_id native_baseline native_listing
  local pending pending_status legacy_status request_id envelope brief_path recovery_pending=0
  local -a native_row_fields=()

  PR_GREEN_NATIVE_ROLLOUT_LISTING=''

  record="$(pr_green_session_record "$project_id" "$pr_number" 2>/dev/null || true)"
  if [[ -z "$record" ]]; then
    pending_status="$(pr_green_delivery_pending_status "$project_id" "$pr_number" 2>/dev/null || true)"
    case "$pending_status" in
      acked) pr_green_delivery_clear_pending "$project_id" "$pr_number" || return 4 ;;
      pending)
        printf '%s\n' "PR $project_id#$pr_number has an unresolved native delivery; suppressing duplicate prompt" >&2
        return 4
        ;;
    esac
    return 1
  fi

  session_id="$(jq -r '.id // empty' <<<"$record")"
  [[ -n "$session_id" ]] || return 1
  terminated="$(jq -r 'if (.isTerminated // false) then "true" else "false" end' <<<"$record")"

  if [[ "$terminated" == "true" ]]; then
    if declare -F pr_green_before_restore_admission >/dev/null 2>&1; then
      pr_green_before_restore_admission "$project_id" "$pr_number" "$session_id" || {
        printf '%s\n' "AO terminated session $session_id is not admitted for restore; suppressing duplicate spawn" >&2
        return 2
      }
    fi
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

  native_row="$(pr_green_session_recovery_row "$project_id" "$session_id" 2>/dev/null || true)"
  IFS='|' read -r -a native_row_fields <<<"$native_row"
  native_workspace="${native_row_fields[0]:-}"
  native_id="${native_row_fields[1]:-}"
  if [[ -z "$native_workspace" || -z "$native_id" ]]; then
    printf '%s\n' "AO session $session_id has no exact native conversation identity; delivery_unconfirmed" >&2
    return 4
  fi
  native_listing="${PR_GREEN_NATIVE_ROLLOUT_LISTING:-}"
  if [[ -z "$native_listing" ]]; then
    native_listing="$(pr_green_find_native_rollouts "$native_workspace" '' "$native_id" 2>/dev/null || true)"
  fi
  [[ -n "$native_listing" ]] || {
    printf '%s\n' "AO session $session_id native rollout is unavailable; delivery_unconfirmed" >&2
    return 4
  }
  legacy_status="$(pr_green_delivery_upgrade_legacy "$project_id" "$pr_number" "$native_listing" "$native_id" "$native_workspace" 2>/dev/null || true)"
  case "$legacy_status" in
    acked)
      pr_green_delivery_clear_pending "$project_id" "$pr_number" || return 4
      ;;
    migrated)
      if [[ "$terminated" == "true" ]]; then
        # The old worker is gone and this exact native identity was restored;
        # reuse the persisted request instead of minting a fresh nonce.
        recovery_pending=1
      else
        # A live legacy delivery may already be sitting in the native composer;
        # the scoped Enter-only proof below can submit it without appending text.
        recovery_pending=1
      fi
      ;;
  esac
  pending_status="$(pr_green_delivery_pending_status "$project_id" "$pr_number" 2>/dev/null || true)"
  case "$pending_status" in
    acked) pr_green_delivery_clear_pending "$project_id" "$pr_number" || return 4 ;;
    pending)
      if [[ "$recovery_pending" -ne 1 ]]; then
        if [[ "$terminated" != "true" ]] && pr_green_delivery_submit_pending_recovery "$project_id" "$pr_number"; then
          printf '%s\n' reused
          return 0
        fi
        printf '%s\n' "PR $project_id#$pr_number has an unresolved native delivery; suppressing duplicate prompt" >&2
        return 4
      fi
      if [[ "$terminated" != "true" ]] && pr_green_delivery_submit_pending_recovery "$project_id" "$pr_number"; then
        printf '%s\n' reused
        return 0
      fi
      if [[ "$terminated" != "true" ]]; then
        printf '%s\n' "PR $project_id#$pr_number has an unresolved native delivery; suppressing duplicate prompt" >&2
        return 4
      fi
      ;;
  esac
  if [[ "$recovery_pending" -eq 1 ]]; then
    pending="$(cat -- "$(pr_green_delivery_pending_path "$project_id" "$pr_number")" 2>/dev/null || true)"
    prompt="$(jq -r '.prompt // empty' <<<"$pending" 2>/dev/null || true)"
    request_id="$(jq -r '.request_id // empty' <<<"$pending" 2>/dev/null || true)"
    brief_path="$(jq -r '.brief_path // empty' <<<"$pending" 2>/dev/null || true)"
    envelope="$(jq -r '.envelope // empty' <<<"$pending" 2>/dev/null || true)"
    [[ -n "$prompt" && -n "$request_id" && -n "$brief_path" && -n "$envelope" ]] || return 4
  else
    request_id="$(pr_green_delivery_request_id)"
    brief_path="$(pr_green_delivery_write_brief "$project_id" "$pr_number" "$request_id" "$prompt" 2>/dev/null || true)"
    [[ -n "$brief_path" ]] || {
      printf '%s\n' "AO session $session_id could not persist immutable repair brief; suppressing prompt" >&2
      return 4
    }
    envelope="$(pr_green_delivery_envelope "$brief_path" "$request_id")"
  fi
  native_baseline="$(pr_green_native_prompt_count "$native_listing" "$envelope" 2>/dev/null || true)"
  if [[ ! "$native_baseline" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "AO session $session_id native rollout is unavailable; delivery_unconfirmed" >&2
    return 4
  fi

  if [[ "$recovery_pending" -ne 1 ]]; then
    if ! pr_green_delivery_write_pending "$project_id" "$pr_number" "$session_id" "$native_id" "$native_workspace" "$prompt" "$envelope" "$request_id" "$brief_path"; then
      printf '%s\n' "AO session $session_id could not persist delivery state; suppressing prompt" >&2
      return 4
    fi
  fi
  if ! ao send --session "$session_id" --message "$envelope" >/dev/null 2>&1; then
    printf '%s\n' "AO session $session_id did not accept the prompt after reuse/restore; suppressing duplicate spawn" >&2
    return 2
  fi
  if pr_green_wait_for_native_prompt "$native_listing" "$envelope" "$native_baseline"; then
    pr_green_delivery_clear_pending "$project_id" "$pr_number" || return 4
    return 0
  fi
  if pr_green_delivery_submit_pending_recovery "$project_id" "$pr_number"; then
    return 0
  fi
  printf '%s\n' "AO session $session_id transport returned success without a native user turn; delivery_unconfirmed" >&2
  return 4
}
