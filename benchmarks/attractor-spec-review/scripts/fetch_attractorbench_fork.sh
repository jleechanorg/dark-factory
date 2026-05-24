#!/usr/bin/env bash
# Fetch or refresh an AttractorBench fork for spec import experiments.
# Usage: ./fetch_attractorbench_fork.sh <target_dir> [repo_url]

set -euo pipefail

TARGET_DIR="${1:?target_dir required}"
REPO_URL="${2:-${ATTRACTORBENCH_REPO:-https://github.com/strongdm/attractorbench.git}}"

mkdir -p "$(dirname "$TARGET_DIR")"

if [ -d "$TARGET_DIR/.git" ]; then
    echo "Updating existing repo at $TARGET_DIR"
    cd "$TARGET_DIR"
    git remote set-url origin "$REPO_URL"
    git fetch --all --prune
    git checkout main
    git pull --ff-only
else
    echo "Cloning $REPO_URL into $TARGET_DIR"
    git clone --depth 1 "$REPO_URL" "$TARGET_DIR"
    cd "$TARGET_DIR"
fi

CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
LATEST_SHA="$(git rev-parse --short HEAD)"

echo "Repository ready:"
echo "  path:   $TARGET_DIR"
echo "  branch: $CURRENT_BRANCH"
echo "  head:   $LATEST_SHA"
