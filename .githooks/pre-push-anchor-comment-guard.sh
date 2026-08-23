#!/usr/bin/env bash
# Pre-push guard: verify anchor comment syntax in pushed commits (Candidate B).
# Ensures Rust files use `//` (not `#`), and Python/Shell files use `#` (not `//`).

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
CHECKER="$REPO_ROOT/scripts/check_anchor_comments.py"

if [ ! -f "$CHECKER" ]; then
  exit 0
fi

# Pick python binary
if [[ -x "$REPO_ROOT/.venv/bin/python" ]]; then
  PYTHON="$REPO_ROOT/.venv/bin/python"
else
  PYTHON=python3
fi

# Check diff between base branch and HEAD
base_ref="origin/main"
if git rev-parse --verify "$base_ref" >/dev/null 2>&1; then
  "$PYTHON" "$CHECKER" --diff-range "${base_ref}...HEAD"
else
  "$PYTHON" "$CHECKER" --check-staged
fi
