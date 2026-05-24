#!/bin/bash
# Run one Attractor Spec Review candidate through the runner.
# Usage: ./run_candidate.sh <method> <run_id> <workdir>

set -euo pipefail

if [ -z "${1:-}" ] || [ -z "${2:-}" ] || [ -z "${3:-}" ]; then
    echo "Error: method, run_id, and workdir required"
    echo "Usage: $0 <review-slim|review-full> <run_id> <workdir>"
    exit 1
fi

METHOD="$1"
RUN_ID="$2"
WORKDIR="$3"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(cd "$BENCHMARK_DIR/../.." && pwd)"
RESULTS_DIR="$WORKDIR/results"

declare -A PIPELINES=(
    ["review-slim"]="benchmarks/attractor-spec-review/pipelines/review_slim.dot"
    ["review-full"]="benchmarks/attractor-spec-review/pipelines/review_full.dot"
    ["slim"]="benchmarks/attractor-spec-review/pipelines/review_slim.dot"
    ["full"]="benchmarks/attractor-spec-review/pipelines/review_full.dot"
)

if [ -z "${PIPELINES[$METHOD]:-}" ]; then
    echo "ERROR: Unknown method: $METHOD"
    echo "Valid methods: slim, full, review-slim, review-full"
    exit 1
fi

PIPELINE="${PIPELINES[$METHOD]}"
PIPELINE_PATH="$PROJECT_ROOT/$PIPELINE"
if [ ! -f "$PIPELINE_PATH" ]; then
    echo "ERROR: Pipeline not found: $PIPELINE_PATH"
    exit 1
fi

if [ ! -d "$WORKDIR" ]; then
    echo "ERROR: Workdir does not exist. Run prepare_candidate.sh first."
    echo "Expected: $WORKDIR"
    exit 1
fi

PYTHON_BIN="python"
if [ -x "$PROJECT_ROOT/.venv/bin/python" ]; then
    PYTHON_BIN="$PROJECT_ROOT/.venv/bin/python"
fi

mkdir -p "$RESULTS_DIR"

TIMESTAMP="$(date +%Y%m%d_%H%M%S)"
LOG_FILE="$RESULTS_DIR/${METHOD}_${RUN_ID}_${TIMESTAMP}.log"
RESULT_FILE="$RESULTS_DIR/${METHOD}_${RUN_ID}_${TIMESTAMP}.json"
CXDB="$RESULTS_DIR/${RUN_ID}.sqlite"

GOAL="Validate spec quality with line-aware checks and independent reviewer feedback"

echo "============================================"
echo "Running Attractor Spec Review"
echo "============================================"
echo "Method:    $METHOD"
echo "Run ID:    $RUN_ID"
echo "Workdir:   $WORKDIR"
echo "Pipeline:  $PIPELINE"
echo "Log:       $LOG_FILE"
echo "Started:   $(date)"

set +e
cd "$PROJECT_ROOT"
$PYTHON_BIN -m runner \
    --pipeline "$PIPELINE" \
    --goal "$GOAL" \
    --backend codex \
    --workdir "$WORKDIR" \
    --feature attractor-spec-review \
    --cxdb "$CXDB" \
    2>&1 | tee "$LOG_FILE"
EXIT_CODE=$?
set -e

if [ $EXIT_CODE -eq 0 ]; then
    RESULT_STATUS="success"
else
    RESULT_STATUS="failed"
fi

cat > "$RESULT_FILE" <<EOF
{
  "method": "$METHOD",
  "run_id": "$RUN_ID",
  "timestamp": "$TIMESTAMP",
  "status": "$RESULT_STATUS",
  "exit_code": $EXIT_CODE,
  "pipeline": "$PIPELINE",
  "workdir": "$WORKDIR",
  "log": "$LOG_FILE",
  "cxdb": "$CXDB"
}
EOF

echo "Finished: $(date)"
echo "Result:   $RESULT_FILE"
echo "Exit:     $EXIT_CODE"
exit $EXIT_CODE
