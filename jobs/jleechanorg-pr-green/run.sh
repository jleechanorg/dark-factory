#!/usr/bin/env bash
set -euo pipefail

# Repository-owned entry point. The host implementation is deliberately
# selected through an explicit variable so deployments cannot silently pick a
# different scheduler. Dark Factory owns the contract; systemd owns cadence.
implementation="${PR_GREEN_IMPLEMENTATION:-$HOME/bin/jleechanorg-pr-green-daily.sh}"
if [[ ! -x "$implementation" ]]; then
  echo "jleechanorg-pr-green implementation missing: $implementation" >&2
  exit 127
fi
exec "$implementation" "$@"
