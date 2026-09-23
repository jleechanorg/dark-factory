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
mkdir -p "$mock_bin" "$worktree"

git init -q --bare "$remote"
git init -q -b pr-123 "$worktree"
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
jq -cn --arg oid "$PUSH_RECEIPT_HEAD_OID" '{headRefName:"pr-123",headRefOid:$oid,headRepository:{nameWithOwner:"jleechanorg/example-repo"},baseRefName:"main"}'
EOF
chmod +x "$mock_bin/gh"

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

output="$(PATH="$mock_bin:$PATH" PUSH_RECEIPT_HEAD_OID="$after_sha" "$helper" "${common[@]}")"
rg -q '^branch=pr-123$' <<<"$output"
rg -q '^upstream=origin/pr-123$' <<<"$output"
rg -q '^target=origin:refs/heads/pr-123$' <<<"$output"
[[ "$(git -C "$remote" rev-parse refs/heads/pr-123)" == "$after_sha" ]]
[[ "$(wc -l <"$ledger")" -eq 1 ]]
jq -e --arg repo example-repo --argjson number 123 --arg session ao-session-123 \
  --arg before "$before_sha" --arg after "$after_sha" \
  'select(.repo == $repo and .number == $number and .session_id == $session and .before_sha == $before and .after_sha == $after and .push_exit_code == 0 and .verified == true and (.pushed_at | type) == "number" and (.commit_url | contains($after)))' \
  "$ledger" >/dev/null

# A second invocation must refuse an already-updated remote rather than record
# an up-to-date pseudo-receipt.
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_HEAD_OID="$after_sha" "$helper" "${common[@]}" >/dev/null 2>&1; then
  printf 'up-to-date push unexpectedly produced a receipt\n' >&2
  exit 1
fi
[[ "$(wc -l <"$ledger")" -eq 1 ]]

# A stale before SHA and a PR-head mismatch both fail before git push.
stale=("${common[@]}")
for i in "${!stale[@]}"; do
  [[ "${stale[$i]}" == --before-sha ]] && stale[i + 1]="$after_sha"
done
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_HEAD_OID="$after_sha" "$helper" "${stale[@]}" >/dev/null 2>&1; then
  printf 'stale before SHA unexpectedly passed\n' >&2
  exit 1
fi
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_HEAD_OID="$before_sha" "$helper" "${common[@]}" >/dev/null 2>&1; then
  printf 'PR head mismatch unexpectedly passed\n' >&2
  exit 1
fi
[[ "$(wc -l <"$ledger")" -eq 1 ]]

# A transport failure must not create a receipt. The bare remote rejects this
# otherwise-valid normal update, while the local/PR identity checks still pass.
before_rejected="$after_sha"
printf 'rejected\n' >>"$worktree/state.txt"
git -C "$worktree" commit -qam rejected
after_rejected="$(git -C "$worktree" rev-parse HEAD)"
cat >"$remote/hooks/pre-receive" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$remote/hooks/pre-receive"
rejected=(
  --repo example-repo --number 123 --session-id ao-session-123
  --before-sha "$before_rejected" --after-sha "$after_rejected"
  --commit-url "https://github.com/jleechanorg/example-repo/commit/$after_rejected"
  --worktree "$worktree" --ledger "$ledger"
)
if PATH="$mock_bin:$PATH" PUSH_RECEIPT_HEAD_OID="$after_rejected" "$helper" "${rejected[@]}" >/dev/null 2>&1; then
  printf 'rejected remote update unexpectedly produced a receipt\n' >&2
  exit 1
fi
[[ "$(wc -l <"$ledger")" -eq 1 ]]
[[ "$(git -C "$remote" rev-parse refs/heads/pr-123)" == "$before_rejected" ]]

printf 'push receipt tests passed\n'
