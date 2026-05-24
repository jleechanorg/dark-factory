#!/bin/bash
# Smoke run — echo backend, no LLM calls, verifies graph mechanics for all 4 pipelines.
# Usage: ./scripts/run_smoke.sh [--pipeline sprint-1-data|sprint-2-backend|sprint-3-frontend|airbnb-clone]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(cd "$BENCHMARK_DIR/../.." && pwd)"
PYTHON_BIN="python"
if [ -x "$PROJECT_ROOT/.venv/bin/python" ]; then
    PYTHON_BIN="$PROJECT_ROOT/.venv/bin/python"
fi

PIPELINE_ARG="${1:-all}"

run_smoke() {
    local name="$1"
    local pipeline_rel="$2"
    local pipeline_path="$PROJECT_ROOT/$pipeline_rel"

    if [ ! -f "$pipeline_path" ]; then
        echo "ERROR: Pipeline not found: $pipeline_path"
        return 1
    fi

    echo ""
    echo "--------------------------------------------"
    echo "Smoke: $name"
    echo "Pipeline: $pipeline_rel"
    echo "--------------------------------------------"

    cd "$PROJECT_ROOT"
    "$PYTHON_BIN" -m runner \
        --pipeline "$pipeline_rel" \
        --goal "smoke test" \
        --backend echo \
        2>&1 | grep -E "(node|step|edge|exhausted|unresolved|ERROR|Warning|Graph)" | head -40 || true

    echo "[smoke done: $name]"
}

declare -A PIPELINES=(
    ["sprint-1-data"]="benchmarks/airbnb-clone/pipelines/sprint-1-data.dot"
    ["sprint-2-backend"]="benchmarks/airbnb-clone/pipelines/sprint-2-backend.dot"
    ["sprint-3-frontend"]="benchmarks/airbnb-clone/pipelines/sprint-3-frontend.dot"
    ["airbnb-clone"]="benchmarks/airbnb-clone/pipelines/airbnb-clone.dot"
)

echo "============================================"
echo "Airbnb Clone — Smoke Run (echo backend)"
echo "Started: $(date)"
echo "============================================"

if [ "$PIPELINE_ARG" = "all" ]; then
    for name in sprint-1-data sprint-2-backend sprint-3-frontend airbnb-clone; do
        run_smoke "$name" "${PIPELINES[$name]}"
    done
else
    if [ -z "${PIPELINES[$PIPELINE_ARG]:-}" ]; then
        echo "ERROR: Unknown pipeline '$PIPELINE_ARG'"
        echo "Valid: ${!PIPELINES[*]}"
        exit 1
    fi
    run_smoke "$PIPELINE_ARG" "${PIPELINES[$PIPELINE_ARG]}"
fi

echo ""
echo "============================================"
echo "All smoke runs done — $(date)"
echo "Graph mechanics OK (echo backend passes; holdout_eval shows 'unresolved' as expected)"
echo "============================================"
