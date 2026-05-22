#!/bin/bash
# Run All Methods - Execute all four orchestration methods against the same spec
# Usage: ./scripts/run_all.sh <spec_path> [--output-dir <results_dir>]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"
OUTPUT_DIR="${2:-${BENCHMARK_DIR}/results}"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

# Validate spec path
if [ -z "${1:-}" ]; then
    echo "Error: spec_path required"
    echo "Usage: $0 <spec_path> [--output-dir <results_dir>]"
    exit 1
fi

SPEC_PATH="$(realpath "$1")"

if [ ! -f "$SPEC_PATH" ]; then
    echo "Error: spec file not found: $SPEC_PATH"
    exit 1
fi

mkdir -p "$OUTPUT_DIR"

# Define methods and their pipelines
declare -A PIPELINES=(
    ["dark-factory"]="pipelines/amazon-clone/main.dot"
    ["df-slim"]="pipelines/slim/main.dot"
    ["kilroy"]="pipelines/kilroy/main.dot"
    ["tracker"]="pipelines/tracker/main.dot"
)

echo "============================================"
echo "Amazon Clone MVP Benchmark"
echo "============================================"
echo "Spec: $SPEC_PATH"
echo "Output: $OUTPUT_DIR"
echo "Started: $(date)"
echo ""

# Run each method
for method in "${!PIPELINES[@]}"; do
    pipeline="${PIPELINES[$method]}"
    result_file="${OUTPUT_DIR}/${method}_${TIMESTAMP}.json"

    echo "--------------------------------------------"
    echo "Running: $method"
    echo "Pipeline: $pipeline"
    echo "Output: $result_file"
    echo ""

    if [ ! -f "${BENCHMARK_DIR}/../${pipeline}" ]; then
        echo "WARNING: Pipeline not found: ${pipeline}, skipping..."
        echo "{\"method\": \"$method\", \"status\": \"skipped\", \"reason\": \"pipeline_not_found\"}" > "$result_file"
        continue
    fi

    set +e
    python -m runner \
        --pipeline "${pipeline}" \
        --goal "$(cat "$SPEC_PATH")" \
        --backend echo \
        --cxdb "${OUTPUT_DIR}/cxdb.sqlite" 2>&1 | tee "${OUTPUT_DIR}/${method}_${TIMESTAMP}.log"

    EXIT_CODE=$?
    set -e

    if [ $EXIT_CODE -eq 0 ]; then
        echo "{\"method\": \"$method\", \"status\": \"success\", \"result_file\": \"$result_file\"}" > "$result_file"
    else
        echo "{\"method\": \"$method\", \"status\": \"failed\", \"exit_code\": $EXIT_CODE}" > "$result_file"
    fi

    echo ""
done

echo "============================================"
echo "All methods completed"
echo "Results saved to: $OUTPUT_DIR"
echo "Finished: $(date)"
echo "============================================"