#!/bin/bash
# Run Candidate - Execute a single benchmark run with proper environment variables
# Usage: ./run_candidate.sh <method> <run_id> <workdir>

set -euo pipefail

if [ -z "${1:-}" ] || [ -z "${2:-}" ] || [ -z "${3:-}" ]; then
    echo "Error: method, run_id, and workdir required"
    echo "Usage: $0 <method> <run_id> <workdir>"
    exit 1
fi

METHOD="$1"
RUN_ID="$2"
WORKDIR="$3"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(cd "$BENCHMARK_DIR/../.." && pwd)"
RESULTS_DIR="$WORKDIR/results"

# Define methods and their pipeline files
declare -A PIPELINES=(
    ["dark-factory"]="benchmarks/amazon-clone/pipelines/dark_factory.dot"
    ["df-slim"]="benchmarks/amazon-clone/pipelines/slim.dot"
    ["kilroy"]="benchmarks/amazon-clone/pipelines/kilroy.dot"
    ["tracker"]="benchmarks/amazon-clone/pipelines/tracker.dot"
)

echo "============================================"
echo "Running Candidate"
echo "============================================"
echo "Method: $METHOD"
echo "Run ID: $RUN_ID"
echo "Workdir: $WORKDIR"
echo "Results: $RESULTS_DIR"
echo ""

# Validate method
if [ -z "${PIPELINES[$METHOD]:-}" ]; then
    echo "ERROR: Unknown method: $METHOD"
    echo "Valid methods: ${!PIPELINES[@]}"
    exit 1
fi

PIPELINE="${PIPELINES[$METHOD]}"
PIPELINE_PATH="${PROJECT_ROOT}/${PIPELINE}"

# Validate pipeline exists
if [ ! -f "$PIPELINE_PATH" ]; then
    echo "ERROR: Pipeline not found: $PIPELINE_PATH"
    exit 1
fi

# Set dark-factory holdouts path
export DARK_FACTORY_HOLDOUTS="$HOME/projects/dark-factory-holdouts"

# Create results directory
mkdir -p "$RESULTS_DIR"

# Read goal from spec
GOAL_FILE="$WORKDIR/spec/feature.md"
if [ ! -f "$GOAL_FILE" ]; then
    echo "ERROR: Goal file not found: $GOAL_FILE"
    exit 1
fi

GOAL=$(cat "$GOAL_FILE")

# Build result file name
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULT_FILE="$RESULTS_DIR/${METHOD}_${RUN_ID}_${TIMESTAMP}.json"
LOG_FILE="$RESULTS_DIR/${METHOD}_${RUN_ID}.log"

echo "Pipeline: $PIPELINE_PATH"
echo "Goal: ${GOAL:0:100}..."
echo "Result: $RESULT_FILE"
echo "Started: $(date)"
echo ""

# Run dark-factory runner
set +e
cd "$PROJECT_ROOT"
python -m runner \
    --pipeline "$PIPELINE" \
    --workdir "$WORKDIR" \
    --goal "$GOAL" \
    --backend ao \
    --cxdb "$RESULTS_DIR/cxdb.sqlite" 2>&1 | tee "$LOG_FILE"

EXIT_CODE=$?
set -e

echo ""
echo "============================================"
echo "Run completed"
echo "Exit code: $EXIT_CODE"
echo "Finished: $(date)"
echo "============================================"

# Write result JSON
if [ $EXIT_CODE -eq 0 ]; then
    cat > "$RESULT_FILE" <<EOF
{
  "method": "$METHOD",
  "run_id": "$RUN_ID",
  "timestamp": "$TIMESTAMP",
  "status": "success",
  "exit_code": 0,
  "workdir": "$WORKDIR",
  "log_file": "$LOG_FILE"
}
EOF
else
    cat > "$RESULT_FILE" <<EOF
{
  "method": "$METHOD",
  "run_id": "$RUN_ID",
  "timestamp": "$TIMESTAMP",
  "status": "failed",
  "exit_code": $EXIT_CODE,
  "workdir": "$WORKDIR",
  "log_file": "$LOG_FILE"
}
EOF
fi

echo "Result saved to: $RESULT_FILE"
exit $EXIT_CODE