#!/usr/bin/env bash
# Resolve AO CLI for factory spawn. ao-ts is the working product CLI for worldarchitect wa-* sessions.
# ao-go at ~/bin/ao-go is the backend daemon binary only — not spawn/session ls.
set -euo pipefail
if [ -n "${AO_BIN:-}" ] && [ -x "$AO_BIN" ]; then
  printf '%s\n' "$AO_BIN"
  exit 0
fi
if [ -x "${HOME}/bin/ao-ts" ]; then
  printf '%s\n' "${HOME}/bin/ao-ts"
  exit 0
fi
if [ -x "${HOME}/projects/agent-orchestrator-mirror/backend/cmd/ao/ao" ]; then
  printf '%s\n' "${HOME}/projects/agent-orchestrator-mirror/backend/cmd/ao/ao"
  exit 0
fi
if command -v ao-ts >/dev/null 2>&1; then
  command -v ao-ts
  exit 0
fi
echo "factory-ao-bin: no ao-ts binary (set AO_BIN)" >&2
exit 127
