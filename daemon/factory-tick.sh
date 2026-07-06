#!/usr/bin/env bash
# One deterministic factory intake tick: recover, unstick, GH->bead sync, overlay upsert.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export BR_DB="${BR_DB:-$ROOT/.beads/beads.db}"
br() { command br --db "$BR_DB" "$@"; }
O="$ROOT/daemon/factory-overlay.sh"
I="$ROOT/daemon/factory-intake-from-gh.sh"
# global callpath CLI
"$O" init
"$O" unstick-dispatching
"$O" recover-held
bash "$I"
echo "--- callpath ---"
callpath run dark-factory ${1+"$@"} 2>/dev/null || true
