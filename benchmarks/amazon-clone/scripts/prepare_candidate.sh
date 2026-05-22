#!/bin/bash
# Prepare Candidate - Copy starter to workdir for benchmark run
# Usage: ./prepare_candidate.sh <candidate> <workdir>

set -euo pipefail

if [ -z "${1:-}" ] || [ -z "${2:-}" ]; then
    echo "Error: candidate and workdir required"
    echo "Usage: $0 <candidate> <workdir>"
    exit 1
fi

CANDIDATE="$1"
WORKDIR="$2"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"

echo "============================================"
echo "Preparing candidate: $CANDIDATE"
echo "Workdir: $WORKDIR"
echo "============================================"

# Remove existing workdir
if [ -d "$WORKDIR" ]; then
    echo "Removing existing workdir..."
    rm -rf "$WORKDIR"
fi

# Create parent directory if needed
mkdir -p "$(dirname "$WORKDIR")"

# Copy starter to workdir
echo "Copying starter to workdir..."
cp -r "${BENCHMARK_DIR}/starter" "$WORKDIR"

# Copy spec to workdir
echo "Copying spec to workdir..."
mkdir -p "$WORKDIR/spec"
cp "${BENCHMARK_DIR}/spec.md" "$WORKDIR/spec/feature.md"

# Create results directory
mkdir -p "$WORKDIR/results"

echo ""
echo "Preparation complete for $CANDIDATE"
echo "Workdir: $WORKDIR"
echo "============================================"