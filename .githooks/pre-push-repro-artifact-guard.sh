#!/usr/bin/env bash
# Guard repro-developer artifacts: raw plaintext archives and passphrases must
# not enter git, and committed archives must be LFS pointers.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

bad=0

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  case "$path" in
    *passphrase*|*.passphrase.txt)
      printf 'repro artifact guard: refusing to commit passphrase-like path: %s\n' "$path" >&2
      bad=1
      ;;
    artifacts/repro-developer/*/*.tar|artifacts/repro-developer/*/*.tar.zst|artifacts/repro-developer/*/*.tar.gz)
      if [[ "$path" != *-sanitized.tar.* ]]; then
        printf 'repro artifact guard: raw archive must be encrypted before commit: %s\n' "$path" >&2
        bad=1
      fi
      ;;
  esac
done < <(git diff --cached --name-only --diff-filter=ACM)

while IFS= read -r path; do
  [[ -z "$path" ]] && continue
  if ! git show ":$path" 2>/dev/null | head -n 1 | grep -qx 'version https://git-lfs.github.com/spec/v1'; then
    printf 'repro artifact guard: archive is not staged as an LFS pointer: %s\n' "$path" >&2
    bad=1
  fi
done < <(git diff --cached --name-only --diff-filter=ACM -- 'artifacts/repro-developer/**/*.tar.zst' 'artifacts/repro-developer/**/*.tar.gz' 'artifacts/repro-developer/**/*.gpg')

exit "$bad"
