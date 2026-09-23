#!/usr/bin/env bash

# Durable, evidence-based accounting for the PR-green scheduler. Dispatching an
# agent is not a fix: only a later GitHub read of a new PR head can confirm it.

pr_green_snapshot_path() {
  local state_dir="$1" repo="$2" pr_number="$3"
  printf '%s/%s-%s.json\n' "$state_dir" "${repo//\//_}" "$pr_number"
}

pr_green_write_snapshot() {
  local state_dir="$1" repo="$2" pr_number="$3" snapshot="$4"
  local path tmp
  path="$(pr_green_snapshot_path "$state_dir" "$repo" "$pr_number")"
  mkdir -p "$state_dir"
  tmp="${path}.tmp.$$"
  printf '%s\n' "$snapshot" >"$tmp"
  mv "$tmp" "$path"
}

pr_green_read_snapshot() {
  local path
  path="$(pr_green_snapshot_path "$1" "$2" "$3")"
  [[ -f "$path" ]] && cat "$path"
}

# Return success when an unchanged, concretely blocked PR was already offered
# to a worker recently.  The durable outcomes log is the source of truth: a
# reconciliation entry is emitted on every scan, so it must never renew this
# cooldown.  A head/blocker change and pending CI verification both remain
# fresh work.
pr_green_same_head_cooldown_applies() {
  local outcomes_file="$1" repo="$2" pr_number="$3" current_state="$4" now="$5" cooldown_seconds="$6"
  [[ -s "$outcomes_file" ]] || return 1

  jq -ne --arg repo "$repo" --argjson number "$pr_number" \
    --argjson current "$current_state" --argjson now "$now" --argjson cooldown "$cooldown_seconds" '
      def signature($state): {
        conflicting: ($state.conflicting // false),
        failed_checks: (($state.failed_checks // []) | sort)
      };
      [inputs
       | select(.repo == $repo and .number == $number)
       | select(.session_action != "reconciled" and .session_action != "cooldown_deferred")]
      | sort_by(.ts // 0)
      | last as $latest
      | $latest != null
        and (($latest.ts // 0) >= ($now - $cooldown))
        and ($latest.classification | IN("no_change", "pushed_still_blocked"))
        and ($latest.head_after == $current.head_sha)
        and (signature($latest.blocker_after) == signature($current))
    ' "$outcomes_file" >/dev/null
}

# Classify an exact-current-head state against the blocker snapshot saved before
# AO was contacted. A changed head alone is never a successful repair.
pr_green_classify_outcome() {
  local before="$1" after="$2"
  jq -nr --argjson before "$before" --argjson after "$after" '
    if $before.head_sha == $after.head_sha then "no_change"
    elif ($after.conflicting or (($after.failed_checks | length) > 0)) then "pushed_still_blocked"
    elif $after.pending_checks or $after.verification_pending
      or ((($after.required_checks_missing // []) | length) > 0)
    then "pushed_ci_pending"
    # A head pushed to repair a CI failure can briefly have an empty rollup
    # while GitHub registers the new check-runs.  Do not treat that gap as a
    # green CI result.  Conflict-only repairs are intentionally exempt: they
    # may be complete before a repository has any checks at all.
    elif (($before.failed_checks | length) > 0)
      and (((($after.check_count // 0) == 0) or (($after.successful_completed_checks // 0) == 0)))
    then "pushed_ci_pending"
    else "fixed_confirmed"
    end'
}

# Print the reporting result and whether it has been independently verified.
pr_green_outcome_result() {
  case "$1" in
    fixed_confirmed) printf '%s\n' 'fixed true' ;;
    pushed_still_blocked) printf '%s\n' 'blocked false' ;;
    pushed_ci_pending) printf '%s\n' 'in_progress false' ;;
    no_change) printf '%s\n' 'no_change false' ;;
    *) printf '%s\n' 'dispatch_failed false' ;;
  esac
}

# Emit a compact state from one authoritative gh pr view response. It supports
# the REST-shaped status rollup returned by gh and intentionally treats only
# actionable terminal failures as blockers; pending checks are reported
# separately.  Counts retain whether GitHub has registered and completed
# successful evidence on this exact head. CANCELLED is excluded because GitHub
# keeps superseded workflow runs in the rollup after their successful
# replacement completes. A context can still contain several attempts, so the
# jq projection below collapses each exact name/context to its newest
# timestamped attempt; detailsUrl and other provider IDs make equal-timestamp
# attempts deterministic. GitHub's zero time is treated as absent for queued
# CheckRuns.
pr_green_state_from_pr_json() {
  jq -c '
    def first_nonempty:
      map(select(. != null and . != "" and . != "0001-01-01T00:00:00Z")) | .[0] // "";
    def failed: ["FAILURE","FAILED","ERROR","TIMED_OUT","ACTION_REQUIRED","STARTUP_FAILURE"];
    def state: ([.conclusion, .state] | first_nonempty | ascii_upcase);
    def status: ((.status // "") | ascii_upcase);
    def completed: (status == "" or status == "COMPLETED");
    def context_key:
      if ((.context // "") != "") then ("status:" + .context)
      elif ((.workflowName // "") != "" and (.name // "") != "") then ("check:" + .workflowName + "\u0000" + .name)
      else ("check:" + ([.name, .workflowName] | first_nonempty))
      end;
    def context_label: ([.context, .name, .workflowName] | first_nonempty);
    def attempt_timestamp: ([.startedAt, .createdAt, .updatedAt, .completedAt] | first_nonempty);
    def attempt_identity:
      if ((.detailsUrl // "") | type) == "string"
        and ((.detailsUrl // "") | test("^https://github\\.com/[^/]+/[^/]+/actions/runs/[0-9]+/job/[0-9]+"))
      then (.detailsUrl | capture("^https://github\\.com/[^/]+/[^/]+/actions/runs/(?<run>[0-9]+)/job/(?<job>[0-9]+)")
        | [(.run | tonumber), (.job | tonumber)])
      elif (([.databaseId, .id] | first_nonempty) as $identity
        | (($identity | type) == "number"
          or (($identity | type) == "string" and ($identity | test("^[0-9]+$")))))
      then ([.databaseId, .id] | first_nonempty) as $identity
        | if ($identity | type) == "number" then $identity else ($identity | tonumber) end
      else null
      end;
    def attempt_rank:
      (attempt_identity) as $identity
      | (attempt_timestamp) as $timestamp
      | if $identity != null then [2, $identity, $timestamp]
        elif $timestamp != "" then [1, $timestamp]
        else null
        end;
    def latest_checks:
      reduce ((.statusCheckRollup // [])[]?) as $check ({};
        ($check | context_key) as $key
        | ($check | attempt_identity) as $identity
        | ($check | attempt_rank) as $rank
        | .[$key] as $previous
        | if $previous == null then
            .[$key] = {check: $check, identity: $identity, rank: $rank, ambiguous: false}
          elif ($rank == null and $previous.rank == null) then
            .[$key].ambiguous = true
          elif $rank == null then .
          elif $previous.rank == null then
            .[$key] = {check: $check, identity: $identity, rank: $rank, ambiguous: false}
          elif ($rank > $previous.rank) then
            .[$key] = {check: $check, identity: $identity, rank: $rank, ambiguous: false}
          elif ($rank == $previous.rank and $identity != null and $identity == $previous.identity) then .
          elif ($rank == $previous.rank) then .[$key].ambiguous = true
          else .
          end
      ) | [.[]];
    latest_checks as $checks |
    {
      head_sha: (.headRefOid // .headRefName // ""),
      base_ref_name: (.baseRefName // ""),
      head_repo_name: (.headRepository.nameWithOwner // ""),
      mergeability: ((.mergeable // "") | ascii_upcase),
      conflicting: ((.mergeable == "CONFLICTING") or (.mergeStateStatus == "DIRTY") or (.mergeStateStatus == "CONFLICTING")),
      failed_checks: [$checks[]?
        | select(.ambiguous | not)
        | .check
        | select(state as $state | failed | index($state))
        | context_label],
      check_count: ($checks | length),
      successful_completed_checks: [$checks[]?
        | select(.ambiguous | not)
        | .check
        | select(state == "SUCCESS" and completed)] | length,
      check_statuses: (reduce $checks[] as $check ({};
        ($check.check | context_label) as $label
        | (if $check.ambiguous then "AMBIGUOUS" else ($check.check | state) end) as $status
        | if .[$label] == null then .[$label] = $status
          elif .[$label] == $status then .
          else .[$label] = "MIXED"
          end)),
      pending_checks: (any($checks[]?;
        .ambiguous
        or ((.check | state) == "" and (.check | status) != "COMPLETED")
        or ((.check | state) | IN("PENDING", "EXPECTED", "QUEUED", "IN_PROGRESS", "REQUESTED"))
      ) or (((.mergeable // "") | ascii_upcase) == "UNKNOWN")
        or (((.mergeStateStatus // "") | ascii_upcase) == "UNKNOWN"))
    }'
}

pr_green_contract_view() {
  gh pr view "$1" --json headRefOid,baseRefName,headRepository 2>/dev/null
}

pr_green_contract_pending() {
  local state="$1" reason="$2" required_inventory="${3:-[]}"
  jq -c --argjson required "$required_inventory" --arg reason "$reason" '
    . + {required_checks: [$required[] | .name] | unique,
         required_checks_missing: [$required[] | .name] | unique,
         verification_pending: true, verification_reason: $reason}' <<<"$state"
}

pr_green_check_runs_for_repo() {
  local repo="$1" head_sha="$2"
  gh api --paginate --slurp "/repos/${repo}/commits/${head_sha}/check-runs?per_page=100" 2>/dev/null |
    jq -c 'if type == "array" then [.[].check_runs[]?] else [.check_runs[]?] end'
}

pr_green_apply_required_contract() {
  local repo="$1" state="$2" pr_url="${3:-}"
  local wa_inventory='[]' required_inventory='[]' head_sha base_ref head_repo base_repo base_encoded
  local observed classic_payload rules_payload classic_contexts='[]' classic_checks='[]' rules_checks='[]'
  local classic_rc rules_rc classic_known=0 rules_known=0 base_runs='[]' head_runs='[]' all_runs='[]'
  local app_sources_available=true required_missing final_state post_view post_rc

  [[ -n "$state" ]] || return 0
  [[ "$repo" == "worldarchitect.ai" ]] && wa_inventory='[{"name":"Green Gate","app_id":null},{"name":"Tests Required Gate","app_id":null}]'

  if [[ -z "$pr_url" ]]; then
    if [[ "$repo" != "worldarchitect.ai" ]]; then
      jq -c '. + {verification_pending:true,verification_reason:"required_check_contract_unknown"}' <<<"$state"
      return
    fi
    jq -c --argjson required "$wa_inventory" '. as $root
      | . + {required_checks: [$required[] | .name]}
      | .required_checks_missing=[$required[] | select(($root.check_statuses[.name]) != "SUCCESS") | .name]' <<<"$state"
    return
  fi

  head_sha="$(jq -r '.head_sha // empty' <<<"$state")"
  base_ref="$(jq -r '.base_ref_name // empty' <<<"$state")"
  head_repo="$(jq -r '.head_repo_name // empty' <<<"$state")"
  if [[ -z "$head_sha" || -z "$base_ref" || -z "$head_repo" ]]; then
    if [[ "$repo" == "worldarchitect.ai" ]]; then
      jq -c --argjson required "$wa_inventory" '. as $root | . + {required_checks: [$required[] | .name]} | .required_checks_missing=[$required[] | select(($root.check_statuses[.name]) != "SUCCESS") | .name]' <<<"$state"
    else
      pr_green_contract_pending "$state" required_check_contract_unavailable "$wa_inventory"
    fi
    return
  fi
  observed="$(pr_green_contract_view "$pr_url" || true)"
  [[ -n "$observed" ]] || { pr_green_contract_pending "$state" required_check_contract_unavailable "$wa_inventory"; return; }
  if ! jq -e --arg head "$head_sha" --arg base "$base_ref" --arg repo "$head_repo" '
      (.headRefOid // "") == $head and (.baseRefName // "") == $base
      and (.headRepository.nameWithOwner // "") == $repo' <<<"$observed" >/dev/null; then
    pr_green_contract_pending "$state" pr_head_changed_during_verification "$wa_inventory"; return
  fi

  base_repo="${pr_url#https://github.com/}"
  base_repo="${base_repo%%/pull/*}"
  base_encoded="$(jq -nr --arg branch "$base_ref" '$branch | @uri')" || { pr_green_contract_pending "$state" required_check_contract_unavailable "$wa_inventory"; return; }
  [[ "$base_repo" != "$pr_url" && "$base_repo" == */* ]] || { pr_green_contract_pending "$state" required_check_contract_unavailable "$wa_inventory"; return; }

  if classic_payload="$(gh api "/repos/${base_repo}/branches/${base_encoded}/protection" 2>/dev/null)"; then classic_rc=0; else classic_rc=$?; fi
  if rules_payload="$(gh api "/repos/${base_repo}/rules/branches/${base_encoded}" 2>/dev/null)"; then rules_rc=0; else rules_rc=$?; fi

  if (( classic_rc == 0 )); then
    if jq -e '
        type == "object" and has("required_status_checks") and
        (.required_status_checks == null or ((.required_status_checks | type) == "object"
          and ((.required_status_checks.contexts // []) | type) == "array"
          and ((.required_status_checks.checks // []) | type) == "array"
          and all((.required_status_checks.contexts // [])[]; type == "string")
          and all((.required_status_checks.checks // [])[]; type == "object" and (.context | type) == "string"
            and ((.app_id == null) or (.app_id | type) == "number"))))' <<<"$classic_payload" >/dev/null; then
      classic_known=1
      classic_contexts="$(jq -c '.required_status_checks.contexts // []' <<<"$classic_payload")"
      classic_checks="$(jq -c '.required_status_checks.checks // []' <<<"$classic_payload")"
    fi
  elif [[ "$(jq -r '.message // empty' <<<"$classic_payload" 2>/dev/null || true)" == "Branch not protected" ]]; then
    classic_known=1
  fi

  if (( rules_rc == 0 )) && jq -e '
      type == "array" and all(.[]; (.type != "required_status_checks") or
        ((.parameters | type) == "object" and ((.parameters.required_status_checks // []) | type) == "array"
          and all((.parameters.required_status_checks // [])[]; type == "object" and (.context | type) == "string"
            and ((.integration_id == null) or (.integration_id | type) == "number"))))' <<<"$rules_payload" >/dev/null; then
    rules_known=1
    rules_checks="$(jq -c '[.[] | select(.type == "required_status_checks")
      | .parameters.required_status_checks[]? | {name:.context,app_id:(.integration_id // null)}]' <<<"$rules_payload")"
  fi
  if (( classic_known == 0 || rules_known == 0 )); then
    pr_green_contract_pending "$state" required_check_contract_unavailable "$wa_inventory"; return
  fi

  required_inventory="$(jq -cn --argjson contexts "$classic_contexts" --argjson checks "$classic_checks" \
      --argjson rules "$rules_checks" --argjson wa "$wa_inventory" '
      ([$contexts[] | {name:.,app_id:null}]
       + [$checks[] | {name:.context,app_id:(.app_id // null)}]
       + $rules + $wa)
      | map(select(.name != "")) | unique_by([.name, (.app_id // null)])')" || { pr_green_contract_pending "$state" required_check_contract_unavailable "$wa_inventory"; return; }

  if [[ "$(jq '[.[] | select(.app_id != null)] | length' <<<"$required_inventory")" != 0 ]]; then
    base_runs="$(pr_green_check_runs_for_repo "$base_repo" "$head_sha")" || { base_runs='[]'; app_sources_available=false; }
    if [[ "$head_repo" != "$base_repo" ]]; then
      head_runs="$(pr_green_check_runs_for_repo "$head_repo" "$head_sha")" || { head_runs='[]'; app_sources_available=false; }
    fi
    all_runs="$(jq -cn --argjson base "$base_runs" --argjson head "$head_runs" '$base + $head')" || { all_runs='[]'; app_sources_available=false; }
  fi

  required_missing="$(jq -cn --argjson required "$required_inventory" --arg head_sha "$head_sha" \
      --argjson statuses "$(jq -c '.check_statuses // {}' <<<"$state")" \
      --argjson runs "$all_runs" --argjson app_sources_available "$app_sources_available" '
      [$required[] | . as $item | select(if ($item.app_id == null) then
        (($statuses[$item.name] // "") != "SUCCESS")
      else (($app_sources_available | not) or (([
        $runs[]? | select((.head_sha // "") == $head_sha)
        | select((.name // "") == $item.name)
        | select(($item.app_id == -1 and (.app.id // null) != null)
          or ($item.app_id != -1 and (.app.id // null) != null
            and ((.app.id | tostring) == ($item.app_id | tostring))))
      ] | sort_by([(.started_at // ""),(.completed_at // ""),(.id // 0)] ) | last
      | ((.status // "") | ascii_downcase) == "completed"
        and ((.conclusion // "") | ascii_downcase) == "success") | not)) end) | $item.name] | unique')" || { pr_green_contract_pending "$state" required_check_contract_unavailable "$required_inventory"; return; }
  final_state="$(jq -c --argjson required "$required_inventory" --argjson missing "$required_missing" \
      --argjson app_sources_available "$app_sources_available" '
      . + {required_checks: [$required[] | .name] | unique,
            required_checks_missing: $missing}
      | if ($app_sources_available | not) then
          . + {verification_pending:true,verification_reason:"required_check_contract_unavailable"}
        elif ($missing | length) > 0 then
          . + {verification_pending:true,verification_reason:"required_checks_pending"}
        else
          del(.verification_pending,.verification_reason) end' <<<"$state")" || { pr_green_contract_pending "$state" required_check_contract_unavailable "$required_inventory"; return; }

  if post_view="$(pr_green_contract_view "$pr_url" 2>/dev/null)"; then
    post_rc=0
  else
    post_rc=$?
  fi
  if (( post_rc != 0 )) || ! jq -e --arg head "$head_sha" --arg base "$base_ref" --arg repo "$head_repo" '(.headRefOid // "") == $head and (.baseRefName // "") == $base and (.headRepository.nameWithOwner // "") == $repo' <<<"$post_view" >/dev/null; then
    if (( post_rc != 0 )); then
      jq -c '. + {verification_pending:true,verification_reason:"required_check_contract_unavailable"}' <<<"$final_state"
    else
      jq -c '. + {verification_pending:true,verification_reason:"pr_head_changed_during_verification"}' <<<"$final_state"
    fi
    return
  fi
  printf '%s\n' "$final_state"
}

pr_green_fetch_live_state() {
  local url="$1" payload
  payload="$(gh pr view "$url" --json headRefOid,mergeable,mergeStateStatus,statusCheckRollup,baseRefName,headRepository 2>/dev/null)" || return 1
  pr_green_state_from_pr_json <<<"$payload"
}
