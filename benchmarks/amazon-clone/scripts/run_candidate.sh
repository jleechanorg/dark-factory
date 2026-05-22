#!/bin/bash
# Run Single Candidate - Execute a single orchestration method
# Usage: ./scripts/run_candidate.sh <method> <spec_path> [--output-dir <results_dir>]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"
OUTPUT_DIR="${3:-${BENCHMARK_DIR}/results}"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

# Define methods and their pipelines
declare -A PIPELINES=(
    ["dark-factory"]="pipelines/amazon-clone/main.dot"
    ["df-slim"]="pipelines/slim/main.dot"
    ["kilroy"]="pipelines/kilroy/main.dot"
    ["tracker"]="pipelines/tracker/main.dot"
)

# Validate arguments
if [ -z "${1:-}" ] || [ -z "${2:-}" ]; then
    echo "Error: method and spec_path required"
    echo "Usage: $0 <method> <spec_path> [--output-dir <results_dir>]"
    echo ""
    echo "Valid methods: ${!PIPELINES[@]}"
    exit 1
fi

METHOD="$1"
SPEC_PATH="$(realpath "$2")"

if [ ! -f "$SPEC_PATH" ]; then
    echo "Error: spec file not found: $SPEC_PATH"
    exit 1
fi

if [ -z "${PIPELINES[$METHOD]:-}" ]; then
    echo "Error: unknown method: $METHOD"
    echo "Valid methods: ${!PIPELINES[@]}"
    exit 1
fi

PIPELINE="${PIPELINES[$METHOD]}"
RESULT_FILE="${OUTPUT_DIR}/${METHOD}_${TIMESTAMP}.json"

mkdir -p "$OUTPUT_DIR"

echo "============================================"
echo "Running: $METHOD"
echo "============================================"
echo "Spec: $SPEC_PATH"
echo "Pipeline: $PIPELINE"
echo "Output: $RESULT_FILE"
echo "Started: $(date)"
echo ""

if [ ! -f "${BENCHMARK_DIR}/../${PIPELINE}" ]; then
    echo "ERROR: Pipeline not found: ${PIPELINE}"
    echo "{\"method\": \"$METHOD\", \"status\": \"error\", \"reason\": \"pipeline_not_found\"}" > "$RESULT_FILE"
    exit 1
fi

set +e
python -m runner \
    --pipeline "${PIPELINE}" \
    --goal "$(cat "$SPEC_PATH")" \
    --backend echo \
    --cxdb "${OUTPUT_DIR}/cxdb.sqlite" 2>&1 | tee "${OUTPUT_DIR}/${METHOD}_${TIMESTAMP}.log"

EXIT_CODE=$?
set -e

echo ""
echo "============================================"
echo "Completed: $METHOD"
echo "Exit code: $EXIT_CODE"
echo "Finished: $(date)"
echo "============================================"

if [ $EXIT_CODE -eq 0 ]; then
    echo "{\"method\": \"$METHOD\", \"status\": \"success\", \"result_file\": \"$RESULT_FILE\"}" > "$RESULT_FILE"
else
    echo "{\"method\": \"$METHOD\", \"status\": \"failed\", \"exit_code\": $EXIT_CODE}" > "$RESULT_FILE"
fi

exit $EXIT_CODE