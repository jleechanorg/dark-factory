#!/usr/bin/env bash
# Install the bead-JSONL sort pre-commit hook for the current clone.
#
# Idempotent: re-running overwrites the hook with the latest version.
# Per-clone: hooks don't transfer across clones (this is by design in git),
# so every fresh clone needs to run this once.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOKS_DIR="$(git rev-parse --git-path hooks)"
HOOK_FILE="$HOOKS_DIR/pre-commit"

mkdir -p "$HOOKS_DIR"

cat > "$HOOK_FILE" << 'INNER'
#!/usr/bin/env bash
# Pre-commit: sort .beads/issues.jsonl by id ascending (root-cause fix for
# the +1686/-1685 noise seen in PRs that touched the JSONL).
# See scripts/sort_beads_jsonl.py for the canonicalizer.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
SORTER="$REPO_ROOT/scripts/sort_beads_jsonl.py"

if [ ! -x "$SORTER" ]; then
  echo "sort_beads_jsonl.py not found or not executable at $SORTER" >&2
  echo "Re-run install-beads-hook.sh from the repo root to bootstrap." >&2
  exit 0  # Don't block commits if the script is missing
fi

# Only run if the JSONL is staged (avoid work on every commit)
if git diff --cached --name-only | grep -q "^\.beads/issues\.jsonl$"; then
  "$SORTER"
  git add .beads/issues.jsonl
fi
INNER

chmod +x "$HOOK_FILE"
echo "Installed bead-JSONL sort pre-commit hook at $HOOK_FILE"
echo "Test it: stage a change to .beads/issues.jsonl and run git commit"
