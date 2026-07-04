#!/usr/bin/env bash
# safe-push-main.sh — push the current branch's commits to origin/main with
# guaranteed landing verification. Fixes the "silent non-ff push" failure class
# (git push prints 'ok' while origin/main advanced under you, e.g. when a factory
# auto-merge landed a PR mid-work). Rebases onto latest origin/main, pushes, and
# HARD-VERIFIES HEAD == origin/main afterward, exiting non-zero if not.
# Usage: daemon/scripts/safe-push-main.sh   (run from the repo, on a feature branch)
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
branch="$(git branch --show-current)"
[ -n "$branch" ] || { echo "safe-push: detached HEAD — refusing" >&2; exit 2; }
[ "$branch" != "main" ] || { echo "safe-push: on main — use a feature branch" >&2; exit 2; }

git fetch origin main --quiet
git rebase refs/remotes/origin/main >/dev/null 2>&1 || {
  echo "safe-push: rebase onto origin/main hit conflicts — resolve manually, not pushing" >&2
  git rebase --abort 2>/dev/null || true
  exit 3
}
git push origin "HEAD:main" >/dev/null 2>&1 || { echo "safe-push: push rejected" >&2; exit 4; }
git fetch origin main --quiet
local_sha="$(git rev-parse HEAD)"
remote_sha="$(git rev-parse refs/remotes/origin/main)"
if [ "$local_sha" = "$remote_sha" ]; then
  echo "safe-push: VERIFIED landed @ ${local_sha:0:9}"
else
  echo "safe-push: DIVERGED — local ${local_sha:0:9} != origin/main ${remote_sha:0:9} (push did NOT land)" >&2
  exit 5
fi
