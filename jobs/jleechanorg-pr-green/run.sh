#!/usr/bin/env bash
set -euo pipefail

# Repository-owned entry point. Keep the implementation beside this wrapper so
# deployments execute the reviewed, tracked job rather than an untracked copy
# in a user's home directory. An explicit override remains useful for probes.
job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
implementation="${PR_GREEN_IMPLEMENTATION:-$job_dir/jleechanorg-pr-green-daily.sh}"
codex_home="${CODEX_HOME:-${PR_GREEN_CODEX_HOME:-$HOME/.codex-dark-factory}}"
if [[ ! -d "$codex_home" || ! -s "$codex_home/auth.json" ]]; then
  echo "jleechanorg-pr-green requires an existing authenticated CODEX_HOME with auth.json: $codex_home" >&2
  exit 78
fi
export CODEX_HOME="$codex_home"
if [[ ! -x "$implementation" ]]; then
  echo "jleechanorg-pr-green implementation missing: $implementation" >&2
  exit 127
fi
exec "$implementation" "$@"
