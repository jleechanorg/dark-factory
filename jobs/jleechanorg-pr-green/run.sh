#!/usr/bin/env bash
set -euo pipefail

# Repository-owned entry point. Keep the implementation beside this wrapper so
# deployments execute the reviewed, tracked job rather than an untracked copy
# in a user's home directory. An explicit override remains useful for probes.
job_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
implementation="${PR_GREEN_IMPLEMENTATION:-$job_dir/jleechanorg-pr-green-daily.sh}"
if [[ ! -x "$implementation" ]]; then
  echo "jleechanorg-pr-green implementation missing: $implementation" >&2
  exit 127
fi
exec "$implementation" "$@"
