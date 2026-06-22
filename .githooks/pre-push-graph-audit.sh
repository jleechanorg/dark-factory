#!/usr/bin/env bash
# Pre-push guard: re-run runner.graph_audit on the pushed branches.
# Mirrors .github/workflows/ci.yml:35 (the "Graph structural audit (G1/G2 dogfooding)" step).
# Runs in < 1s locally. Bypass with `git push --no-verify`.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Pick the venv python if present, else system python (matches CI behaviour).
if [[ -x .venv/bin/python ]]; then
  PYTHON=.venv/bin/python
else
  PYTHON=python
fi

# Same invocation as CI: python -m runner.graph_audit pipelines
# Exit codes: 0=clean, 1=violations, 2=missing pipelines dir.
# Treat exit 2 as "fail open" with a stderr warning (fresh-clone state, not a real violation).
set +e
"$PYTHON" -m runner.graph_audit pipelines
rc=$?
set -e

if [[ $rc -eq 0 ]]; then
  exit 0
elif [[ $rc -eq 2 ]]; then
  printf 'pre-push graph audit: pipelines/ not found, skipping (fresh-clone state)\n' >&2
  exit 0
else
  printf 'pre-push graph audit: FAILED (exit %d). Hint: bypass with `git push --no-verify` if this is a known-bad graph.\n' "$rc" >&2
  exit 1
fi