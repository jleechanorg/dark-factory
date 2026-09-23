#!/usr/bin/env bash
set -euo pipefail

# Record a push receipt only for a real, non-force update of the exact PR head.
# This is intentionally a job-owned producer: worker prose and outcome claims
# are never trusted as evidence of a remote write.

usage() {
  cat >&2 <<'EOF'
usage: push-receipt.sh --repo REPO --number N --session-id ID \
  --before-sha SHA --after-sha SHA --commit-url URL \
  [--worktree PATH] [--ledger PATH]
EOF
}

fail() {
  printf 'push receipt refused: %s\n' "$1" >&2
  return 1
}

main() {
  local repo='' number='' session_id='' before_sha='' after_sha='' commit_url=''
  local worktree="${PR_GREEN_PUSH_WORKTREE:-${PR_GREEN_AO_WORKTREE:-$PWD}}"
  local ledger="${PR_GREEN_PUSH_RECEIPTS_PATH:-${PR_GREEN_METRICS_DIR:-${TMPDIR:-/tmp}/jleechanorg-pr-green}/push-receipts.jsonl}"
  while (($#)); do
    case "$1" in
      --repo) [[ $# -ge 2 ]] || { usage; return 2; }; repo="$2"; shift 2 ;;
      --number) [[ $# -ge 2 ]] || { usage; return 2; }; number="$2"; shift 2 ;;
      --session-id) [[ $# -ge 2 ]] || { usage; return 2; }; session_id="$2"; shift 2 ;;
      --before-sha) [[ $# -ge 2 ]] || { usage; return 2; }; before_sha="$2"; shift 2 ;;
      --after-sha) [[ $# -ge 2 ]] || { usage; return 2; }; after_sha="$2"; shift 2 ;;
      --commit-url) [[ $# -ge 2 ]] || { usage; return 2; }; commit_url="$2"; shift 2 ;;
      --worktree) [[ $# -ge 2 ]] || { usage; return 2; }; worktree="$2"; shift 2 ;;
      --ledger) [[ $# -ge 2 ]] || { usage; return 2; }; ledger="$2"; shift 2 ;;
      -h|--help) usage; return 0 ;;
      *) usage; return 2 ;;
    esac
  done

  [[ "$repo" =~ ^[[:alnum:]._-]+$ ]] || { fail 'invalid repository name'; return 1; }
  [[ "$number" =~ ^[1-9][0-9]*$ ]] || { fail 'invalid pull request number'; return 1; }
  [[ -n "$session_id" && "$session_id" != *$'\n'* ]] || { fail 'missing session id'; return 1; }
  [[ "$before_sha" =~ ^[0-9a-fA-F]{40}$ && "$after_sha" =~ ^[0-9a-fA-F]{40}$ ]] || { fail 'SHA must be a full 40-character object id'; return 1; }
  [[ "$before_sha" != "$after_sha" ]] || { fail 'before and after SHA are identical'; return 1; }
  [[ "$commit_url" == "https://github.com/jleechanorg/$repo/commit/$after_sha" ]] || { fail 'commit URL does not bind to repo and after SHA'; return 1; }
  [[ -d "$worktree/.git" || -f "$worktree/.git" ]] || { fail "worktree is not a git checkout: $worktree"; return 1; }

  local ledger_dir lock_fd
  ledger_dir="$(dirname -- "$ledger")"
  mkdir -p -- "$ledger_dir" || { fail 'could not create receipt directory'; return 1; }
  exec {lock_fd}>"${ledger}.lock" || { fail 'could not create receipt lock'; return 1; }
  flock -x "$lock_fd" || { fail 'could not acquire receipt lock'; return 1; }
  touch -- "$ledger" || { fail 'receipt ledger is not writable'; return 1; }

  local pr_url="https://github.com/jleechanorg/$repo/pull/$number"
  local pr_json head_branch head_oid head_repo branch upstream remote target_branch remote_url
  pr_json="$(gh pr view "$pr_url" --json headRefName,headRefOid,headRepository,baseRefName 2>/dev/null)" \
    || { fail 'GitHub PR read failed'; return 1; }
  head_branch="$(jq -r '.headRefName // empty' <<<"$pr_json")"
  head_oid="$(jq -r '.headRefOid // empty' <<<"$pr_json")"
  head_repo="$(jq -r '.headRepository.nameWithOwner // empty' <<<"$pr_json")"
  [[ "$head_repo" == "jleechanorg/$repo" ]] || { fail 'PR head repository mismatch'; return 1; }
  [[ "$head_oid" == "$after_sha" ]] || { fail 'GitHub PR head does not equal after SHA'; return 1; }
  [[ -n "$head_branch" && "$head_branch" != *$'\n'* ]] || { fail 'GitHub PR head branch missing'; return 1; }

  branch="$(git -C "$worktree" symbolic-ref --quiet --short HEAD 2>/dev/null || true)"
  [[ "$branch" == "$head_branch" ]] || { fail 'checked-out branch does not equal PR head branch'; return 1; }
  [[ "$(git -C "$worktree" rev-parse HEAD 2>/dev/null || true)" == "$after_sha" ]] || { fail 'local HEAD does not equal after SHA'; return 1; }
  upstream="$(git -C "$worktree" rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)"
  [[ "$upstream" =~ ^([^/]+)/(.+)$ ]] || { fail 'branch has no explicit upstream tracking'; return 1; }
  remote="${BASH_REMATCH[1]}"
  target_branch="${BASH_REMATCH[2]}"
  [[ "$target_branch" == "$branch" ]] || { fail 'upstream branch does not equal checked-out branch'; return 1; }
  remote_url="$(git -C "$worktree" remote get-url "$remote" 2>/dev/null || true)"
  case "${remote_url%.git}" in
    "https://github.com/jleechanorg/$repo"|"git@github.com:jleechanorg/$repo"|"ssh://git@github.com/jleechanorg/$repo") ;;
    *) fail 'upstream remote URL does not match the PR repository'; return 1 ;;
  esac

  printf 'branch=%s\nupstream=%s\ntarget=%s:refs/heads/%s\n' "$branch" "$upstream" "$remote" "$target_branch"

  local remote_before remote_after ls_line push_output push_url
  push_url="$(git -C "$worktree" config --get "remote.$remote.pushurl" 2>/dev/null || true)"
  [[ -n "$push_url" ]] || push_url="$remote"
  ls_line="$(git -C "$worktree" ls-remote --heads "$push_url" "refs/heads/$target_branch" 2>/dev/null || true)"
  remote_before="${ls_line%%$'\t'*}"
  [[ "$remote_before" == "$before_sha" ]] || { fail 'remote PR head does not equal before SHA'; return 1; }
  git -C "$worktree" merge-base --is-ancestor "$before_sha" "$after_sha" \
    || { fail 'after SHA is not a descendant of before SHA; refusing rewrite'; return 1; }

  if [[ -s "$ledger" ]] && jq -e --arg repo "$repo" --argjson number "$number" --arg after "$after_sha" \
    'select(.repo == $repo and .number == $number and .after_sha == $after and .verified == true)' \
    "$ledger" >/dev/null 2>&1; then
    fail 'receipt already exists for this remote head'
    return 1
  fi

  push_output="$(git -C "$worktree" push --porcelain "$remote" "HEAD:refs/heads/$target_branch" 2>&1)" \
    || { printf '%s\n' "$push_output" >&2; fail 'normal git push failed'; return 1; }
  printf '%s\n' "$push_output"
  [[ "$push_output" != *'up to date'* && "$push_output" != *'Everything up-to-date'* ]] \
    || { fail 'git reported an up-to-date ref instead of an update'; return 1; }
  grep -Eq '[0-9a-fA-F]+\.\.[0-9a-fA-F]+' <<<"$push_output" \
    || { fail 'git porcelain output did not prove a ref update'; return 1; }

  ls_line="$(git -C "$worktree" ls-remote --heads "$push_url" "refs/heads/$target_branch" 2>/dev/null || true)"
  remote_after="${ls_line%%$'\t'*}"
  [[ "$remote_after" == "$after_sha" ]] || { fail 'remote ref does not equal after SHA after push'; return 1; }

  local pushed_at receipt
  pushed_at="$(date +%s)"
  receipt="$(jq -cn --arg repo "$repo" --argjson number "$number" --arg session_id "$session_id" \
    --arg before_sha "$before_sha" --arg after_sha "$after_sha" --arg commit_url "$commit_url" \
    --argjson pushed_at "$pushed_at" '{repo:$repo,number:$number,session_id:$session_id,before_sha:$before_sha,after_sha:$after_sha,commit_url:$commit_url,pushed_at:$pushed_at,push_exit_code:0,verified:true}')"
  printf '%s\n' "$receipt" >>"$ledger" || { fail 'receipt ledger append failed'; return 1; }
  printf '%s\n' "$receipt"
}

main "$@"
