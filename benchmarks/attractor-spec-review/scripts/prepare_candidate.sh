#!/bin/bash
# Prepare a candidate workdir for the Attractor Spec Review benchmark.
# Usage: ./prepare_candidate.sh <run_id> <workdir>

set -euo pipefail

if [ -z "${1:-}" ] || [ -z "${2:-}" ]; then
    echo "Error: run_id and workdir are required"
    echo "Usage: $0 <run_id> <workdir>"
    exit 1
fi

RUN_ID="$1"
WORKDIR="$2"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"

echo "============================================"
echo "Preparing Attractor Spec Review candidate"
echo "============================================"
echo "Run ID:  $RUN_ID"
echo "Workdir: $WORKDIR"

if [ -d "$WORKDIR" ]; then
    rm -rf "$WORKDIR"
fi

mkdir -p "$(dirname "$WORKDIR")"

echo "Copying starter..."
cp -r "${BENCHMARK_DIR}/starter" "$WORKDIR"

echo "Copying benchmark runtime artifacts for prompt/path resolution..."
mkdir -p "$WORKDIR/benchmarks/attractor-spec-review"
cp -r "${BENCHMARK_DIR}/prompts" "$WORKDIR/benchmarks/attractor-spec-review/"
cp -r "${BENCHMARK_DIR}/scripts" "$WORKDIR/benchmarks/attractor-spec-review/"
cp -r "${BENCHMARK_DIR}/pipelines" "$WORKDIR/benchmarks/attractor-spec-review/"
cp "${BENCHMARK_DIR}/README.md" "$WORKDIR/benchmarks/attractor-spec-review/README.md"
cp "${BENCHMARK_DIR}/spec.md" "$WORKDIR/benchmarks/attractor-spec-review/spec.md"
cp "${BENCHMARK_DIR}/visible_acceptance.md" "$WORKDIR/benchmarks/attractor-spec-review/visible_acceptance.md"

echo "Copying spec for candidate scope..."
mkdir -p "$WORKDIR/spec"
cp "${BENCHMARK_DIR}/spec.md" "$WORKDIR/spec/feature.md"
cp "${BENCHMARK_DIR}/visible_acceptance.md" "$WORKDIR/spec/visible_acceptance.md"

mkdir -p "$WORKDIR/results"
mkdir -p "$WORKDIR/spec_review"

echo "Setting executable bit on benchmark scripts..."
chmod +x "$WORKDIR/benchmarks/attractor-spec-review/scripts/review_with_codex.sh"
chmod +x "$WORKDIR/benchmarks/attractor-spec-review/scripts/fetch_attractorbench_fork.sh"

mkdir -p "$WORKDIR/results"

echo "Preparation complete."
echo "Results path: $WORKDIR/results"
