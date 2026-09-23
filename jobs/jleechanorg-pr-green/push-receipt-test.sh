#!/usr/bin/env bash
set -euo pipefail

job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
helper="$job_dir/push-receipt.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf -- "$fixture_dir"' EXIT
mock_bin="$fixture_dir/bin"
remote="$fixture_dir/remote.git"
worktree="$fixture_dir/worktree"
ledger="$fixture_dir/push-receipts.jsonl"
gh_calls="$fixture_dir/gh-calls"
real_git="$(command -v git)"
mkdir -p "$mock_bin" "$worktree"

git init -q --bare "$remote"
git init -q -b ao/session-123/pr-123 "$worktree"
git -C "$worktree" config user.email 'push-receipt-test@example.invalid'
git -C "$worktree" config user.name 'push-receipt-test'
printf 'before\n' >"$worktree/state.txt"
git -C "$worktree" add state.txt
git -C "$worktree" commit -qm before
before_sha="$(git -C "$worktree" rev-parse HEAD)"
git -C "$worktree" remote add origin "https://github.com/jleechanorg/example-repo.git"
git -C "$worktree" remote set-url --add --push origin "$remote"
git -C "$worktree" push -q -u origin HEAD:refs/heads/pr-123
printf 'after\n' >>"$worktree/state.txt"
git -C "$worktree" commit -qam after
after_sha="$(git -C "$worktree" rev-parse HEAD)"

cat >"$mock_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1 $2" == 'pr view' ]] || { printf 'unexpected gh call: %s\n' "$*" >&2; exit 1; }
count=0
[[ -s "$PUSH_RECEIPT_GH_CALLS" ]] && count="$(cat "$PUSH_RECEIPT_GH_CALLS")"
count=$((count + 1))
printf '%s\n' "$count" >"$PUSH_RECEIPT_GH_CALLS"
if [[ "$count" -eq 1 ]]; then
  oid="$PUSH_RECEIPT_FIRST_HEAD_SHA"
elif [[ -n "${PUSH_RECEIPT_AFTER_HEAD_SEQUENCE:-}" ]]; then
  IFS=, read -r -a sequence <<<"$PUSH_RECEIPT_AFTER_HEAD_SEQUENCE"
  index=$((count - 2))
  (( index < ${#sequence[@]} )) || index=$((${#sequence[@]} - 1))
  oid="${sequence[$index]}"
else
  oid="$PUSH_RECEIPT_AFTER_HEAD_SHA"
fi
jq -cn --arg oid "$oid" '{headRefName:"pr-123",headRefOid:$oid,headRepository:{nameWithOwner:"jleechanorg/example-repo"},baseRefName:"main"}'
EOF
chmod +x "$mock_bin/gh"
cat >"$mock_bin/git" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == *'remote get-url --push origin'* ]]; then
  printf '%s\n' 'https://github.com/jleechanorg/example-repo.git'
  exit 0
fi
if [[ "$*" == *' push --porcelain '* && "${PUSH_RECEIPT_MUTATE_ON_PUSH:-0}" == 1 ]]; then
  "$PUSH_RECEIPT_REAL_GIT" -C "$PUSH_RECEIPT_WORKTREE" commit --allow-empty -qm 'unexpected head movement'
fi
exec "$PUSH_RECEIPT_REAL_GIT" "$@"
EOF
chmod +x "$mock_bin/git"

common=(
  --repo example-repo
  --number 123
  --session-id ao-session-123
  --before-sha "$before_sha"
  --after-sha "$after_sha"
  --commit-url "https://github.com/jleechanorg/example-repo/commit/$after_sha"
  --worktree "$worktree"
  --ledger "$ledger"
)

trace="$fixture_dir/trace"
: >"$gh_calls"
output="$(PATH="$mock_bin:$PATH" PUSH_RECEIPT_REAL_GIT="$real_git" PUSH_RECEIPT_GH_CALLS="$gh_calls" PUSH_RECEIPT_FIRST_HEAD_SHA="$before_sha" PUSH_RECEIPT_AFTER_HEAD_SHA="$after_sha" "$helper" "${common[@]}" 2>"$trace")"
jq -e --arg repo example-repo --arg after "$after_sha" 'select(.repo == $repo and .after_sha == $after and .verified == true)' <<<"$output" >/dev/null
rg -q '^branch=ao/session-123/pr-123$' "$trace"
rg -q '^upstream=origin/pr-123$' "$trace"
rg -q '^target=origin:refs/heads/pr-123$' "$trace"
rg -q '\.\.' "$trace"
[[ "$(git -C "$remote" rev-parse refs/heads/pr-123)" == "$after_sha" ]]
[[ "$(wc -l <"$ledger")" -eq 1 ]]
jq -e --arg repo example-repo --argjson number 123 --arg session ao-session-123 \
  --arg before "$before_sha" --arg after "$after_sha" \
  'select(.repo == $repo and .number == $number and .session_id == $session and .before_sha == $before and .after_sha == $after and .push_exit_code == 0 and .verified == true and (.pushed_at | type) == "number" and (.commit_url | contains($after)))' \
  "$ledger" >/dev/null

# The push object is pinned after validation. A concurrent local HEAD movement
# immediately before git push must not change the object sent to the PR branch.
before_moved="$after_sha"
printf 'pinned\n' >>"$worktree/state.txt"
git -C "$worktree" commit -qam pinned
after_moved="$(git -C "$worktree" rev-parse HEAD)"
moved=(
  --repo example-repo --number 123 --session-id ao-session-123
  --before-sha "$before_moved" --after-sha "$after_moved"
  --commit-url "https://github.com/jleechanorg/example-repo/commit/$after_moved"
  --worktree "$worktree" --ledger "$ledger"
)
: >"$gh_calls"
if ! PATH="$mock_bin:$PATH" PUSH_RECEIPT_REAL_GIT="$real_git" PUSH_RECEIPT_WORKTREE="$worktree" \
  PUSH_RECEIPT_MUTATE_ON_PUSH=1 PUSH_RECEIPT_GH_CALLS="$gh_calls" \
  PUSH_RECEIPT_FIRST_HEAD_SHA="$before_moved" PUSH_RECEIPT_AFTER_HEAD_SHA="$after_moved" \
  "$helper" "${moved[@]}" >"$fixture_dir/moved-output" 2>"$fixture_dir/moved-trace"; then
  cat "$fixture_dir/moved-trace" >&2
  exit 1
fi
jq -e --arg after "$after_moved" 'select(.after_sha == $after and .verified == true)' \
  "$ledger" >/dev/null
[[ "$(git -C "$remote" rev-parse refs/heads/pr-123)" == "$after_moved" ]]
[[ "$(wc -l <"$ledger")" -eq 2 ]]
# The mutation is only a race fixture; restore the validated checkout for the
# subsequent independent negative cases.
git -C "$worktree" reset --hard -q "$after_moved"

# A second invocation must refuse an already-updated remote rather than record
# an up-to-date pseudo-receipt.
: >"$gh_calls"
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_REAL_GIT="$real_git" PUSH_RECEIPT_GH_CALLS="$gh_calls" PUSH_RECEIPT_FIRST_HEAD_SHA="$before_moved" PUSH_RECEIPT_AFTER_HEAD_SHA="$after_moved" "$helper" "${moved[@]}" >"$fixture_dir/up-to-date-output" 2>"$fixture_dir/up-to-date-trace"; then
  printf 'up-to-date push unexpectedly produced a receipt\n' >&2
  exit 1
fi
rg -q '^push receipt refused: remote PR head does not equal before SHA$' "$fixture_dir/up-to-date-trace" || {
  cat "$fixture_dir/up-to-date-trace" >&2
  exit 1
}
[[ "$(wc -l <"$ledger")" -eq 2 ]]

# A stale before SHA and a PR-head mismatch both fail before git push.
stale=("${moved[@]}")
for i in "${!stale[@]}"; do
  [[ "${stale[$i]}" == --before-sha ]] && stale[i + 1]="$after_moved"
done
: >"$gh_calls"
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_REAL_GIT="$real_git" PUSH_RECEIPT_GH_CALLS="$gh_calls" PUSH_RECEIPT_FIRST_HEAD_SHA="$after_moved" PUSH_RECEIPT_AFTER_HEAD_SHA="$after_moved" "$helper" "${stale[@]}" >"$fixture_dir/stale-output" 2>"$fixture_dir/stale-trace"; then
  printf 'stale before SHA unexpectedly passed\n' >&2
  exit 1
fi
rg -q '^push receipt refused: before and after SHA are identical$' "$fixture_dir/stale-trace"
: >"$gh_calls"
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_REAL_GIT="$real_git" PUSH_RECEIPT_GH_CALLS="$gh_calls" PUSH_RECEIPT_FIRST_HEAD_SHA="$after_moved" PUSH_RECEIPT_AFTER_HEAD_SHA="$after_moved" "$helper" "${moved[@]}" >"$fixture_dir/head-mismatch-output" 2>"$fixture_dir/head-mismatch-trace"; then
  printf 'PR head mismatch unexpectedly passed\n' >&2
  exit 1
fi
rg -q '^push receipt refused: GitHub PR head does not equal before SHA$' "$fixture_dir/head-mismatch-trace"
[[ "$(wc -l <"$ledger")" -eq 2 ]]

# A remote update may be visible before GitHub's PR projection catches up. A
# bounded observation poll accepts the receipt only once the exact SHA appears.
before_post="$after_moved"
printf 'post-read\n' >>"$worktree/state.txt"
git -C "$worktree" commit -qam post-read
after_post="$(git -C "$worktree" rev-parse HEAD)"
post_read=(
  --repo example-repo --number 123 --session-id ao-session-123
  --before-sha "$before_post" --after-sha "$after_post"
  --commit-url "https://github.com/jleechanorg/example-repo/commit/$after_post"
  --worktree "$worktree" --ledger "$ledger"
)
: >"$gh_calls"
if ! PATH="$mock_bin:$PATH" PUSH_RECEIPT_REAL_GIT="$real_git" PUSH_RECEIPT_GH_CALLS="$gh_calls" \
  PUSH_RECEIPT_FIRST_HEAD_SHA="$before_post" PUSH_RECEIPT_AFTER_HEAD_SEQUENCE="$before_post,$after_post" \
  PR_GREEN_PUSH_POST_POLL_ATTEMPTS=3 PR_GREEN_PUSH_POST_POLL_SECONDS=0 \
  "$helper" "${post_read[@]}" >"$fixture_dir/post-read-output" 2>"$fixture_dir/post-read-trace"; then
  cat "$fixture_dir/post-read-trace" >&2
  exit 1
fi
[[ "$(git -C "$remote" rev-parse refs/heads/pr-123)" == "$after_post" ]]
[[ "$(wc -l <"$ledger")" -eq 3 ]]

# A projection that never reaches the exact pushed SHA is a refusal, not a
# second push attempt or a receipt.
before_permanent="$after_post"
printf 'permanent-mismatch\n' >>"$worktree/state.txt"
git -C "$worktree" commit -qam permanent-mismatch
after_permanent="$(git -C "$worktree" rev-parse HEAD)"
permanent=(
  --repo example-repo --number 123 --session-id ao-session-123
  --before-sha "$before_permanent" --after-sha "$after_permanent"
  --commit-url "https://github.com/jleechanorg/example-repo/commit/$after_permanent"
  --worktree "$worktree" --ledger "$ledger"
)
: >"$gh_calls"
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_REAL_GIT="$real_git" PUSH_RECEIPT_GH_CALLS="$gh_calls" \
  PUSH_RECEIPT_FIRST_HEAD_SHA="$before_permanent" PUSH_RECEIPT_AFTER_HEAD_SEQUENCE="$before_permanent" \
  PR_GREEN_PUSH_POST_POLL_ATTEMPTS=3 PR_GREEN_PUSH_POST_POLL_SECONDS=0 \
  "$helper" "${permanent[@]}" >"$fixture_dir/permanent-output" 2>"$fixture_dir/permanent-trace"; then
  printf 'permanent post-push GitHub mismatch unexpectedly produced a receipt\n' >&2
  exit 1
fi
rg -q '^push receipt refused: post-push GitHub PR head verification failed$' "$fixture_dir/permanent-trace"
[[ "$(git -C "$remote" rev-parse refs/heads/pr-123)" == "$after_permanent" ]]
[[ "$(wc -l <"$ledger")" -eq 3 ]]

# A transport failure must not create a receipt. The bare remote rejects this
# otherwise-valid normal update, while the local/PR identity checks still pass.
before_rejected="$after_permanent"
printf 'rejected\n' >>"$worktree/state.txt"
git -C "$worktree" commit -qam rejected
after_rejected="$(git -C "$worktree" rev-parse HEAD)"
cat >"$remote/hooks/pre-receive" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' hook-ran >"$PUSH_RECEIPT_HOOK_MARKER"
exit 1
EOF
chmod +x "$remote/hooks/pre-receive"
rejected=(
  --repo example-repo --number 123 --session-id ao-session-123
  --before-sha "$before_rejected" --after-sha "$after_rejected"
  --commit-url "https://github.com/jleechanorg/example-repo/commit/$after_rejected"
  --worktree "$worktree" --ledger "$ledger"
)
: >"$gh_calls"
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_REAL_GIT="$real_git" PUSH_RECEIPT_GH_CALLS="$gh_calls" PUSH_RECEIPT_FIRST_HEAD_SHA="$before_rejected" PUSH_RECEIPT_AFTER_HEAD_SHA="$after_rejected" PUSH_RECEIPT_HOOK_MARKER="$fixture_dir/hook-marker" "$helper" "${rejected[@]}" >"$fixture_dir/rejected-output" 2>"$fixture_dir/rejected-trace"; then
  printf 'rejected remote update unexpectedly produced a receipt\n' >&2
  exit 1
fi
rg -q '^push receipt refused: normal git push failed$' "$fixture_dir/rejected-trace"
[[ "$(cat "$fixture_dir/hook-marker")" == hook-ran ]]
[[ "$(wc -l <"$ledger")" -eq 3 ]]
[[ "$(git -C "$remote" rev-parse refs/heads/pr-123)" == "$before_rejected" ]]

printf 'push receipt tests passed\n'
