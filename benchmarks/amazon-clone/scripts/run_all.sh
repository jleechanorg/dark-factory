#!/bin/bash
# Run All - Master script for all methods x3 runs
# Usage: ./run_all.sh [--workdir-base <base_dir>]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(cd "$BENCHMARK_DIR/../.." && pwd)"

# Default workdir base
WORKDIR_BASE="${1:-$HOME/benchmark-runs}"

# Methods to run
METHODS=("dark-factory" "df-slim" "kilroy" "tracker")
RUNS="${BENCHMARK_RUNS:-3}"


# Prepare script paths
PREPARE_SCRIPT="$SCRIPT_DIR/prepare_candidate.sh"
RUN_CANDIDATE_SCRIPT="$SCRIPT_DIR/run_candidate.sh"

echo "============================================"
echo "Amazon Clone MVP Benchmark"
echo "Full Method x Run Matrix"
echo "============================================"
echo "Workdir base: $WORKDIR_BASE"
echo "Methods: ${METHODS[*]}"
echo "Runs per method: $RUNS"
echo "Started: $(date)"
echo ""

# Check scripts exist
for script in "$PREPARE_SCRIPT" "$RUN_CANDIDATE_SCRIPT"; do
    if [ ! -f "$script" ]; then
        echo "ERROR: Required script not found: $script"
        exit 1
    fi
done

# Track overall results
declare -A RUN_RESULTS

# Iterate over methods
for METHOD in "${METHODS[@]}"; do
    echo ""
    echo "============================================"
    echo "METHOD: $METHOD"
    echo "============================================"

    # Create results directory for this method
    METHOD_RESULTS="$WORKDIR_BASE/results/${METHOD}"
    mkdir -p "$METHOD_RESULTS"

    for ((run=1; run<=RUNS; run++)); do
        echo ""
        echo "--------------------------------------------"
        echo "Run: $run/$RUNS"
        echo "--------------------------------------------"

        RUN_ID="${METHOD}_run${run}"
        WORKDIR="$WORKDIR_BASE/workdirs/${RUN_ID}"

        # Prepare candidate
        echo "Preparing candidate..."
        bash "$PREPARE_SCRIPT" "$RUN_ID" "$WORKDIR"

        # Run candidate
        echo ""
        echo "Running candidate..."
        set +e
        bash "$RUN_CANDIDATE_SCRIPT" "$METHOD" "$RUN_ID" "$WORKDIR"
        EXIT_CODE=$?
        set -e

        if [ $EXIT_CODE -eq 0 ]; then
            echo "Run $run completed successfully"
            RUN_RESULTS["${METHOD}_${run}"]="success"
        else
            echo "Run $run failed with exit code $EXIT_CODE"
            RUN_RESULTS["${METHOD}_${run}"]="failed:$EXIT_CODE"
        fi

        echo ""
    done

    echo ""
    echo "Completed all runs for: $METHOD"
done

echo ""
echo "============================================"
echo "All methods completed"
echo "============================================"
echo "Workdir base: $WORKDIR_BASE"
echo "Results: $WORKDIR_BASE/results"
echo ""

# Print summary
echo "RUN SUMMARY"
echo "--------------------------------------------"
for method in "${METHODS[@]}"; do
    successes=0
    failures=0
    for ((run=1; run<=RUNS; run++)); do
        key="${method}_${run}"
        result="${RUN_RESULTS[$key]:-unknown}"
        if [[ "$result" == "success" ]]; then
            ((successes++))
        else
            ((failures++))
        fi
    done
    echo "  $method: $successes success, $failures failed"
done
echo "--------------------------------------------"
echo "Finished: $(date)"
echo "============================================"

# Run scoring for each workdir
echo ""
echo "Running scoring..."
for method in "${METHODS[@]}"; do
    for ((run=1; run<=RUNS; run++)); do
        RUN_ID="${method}_run${run}"
        WORKDIR="$WORKDIR_BASE/workdirs/${RUN_ID}"
        SCORE_FILE="$WORKDIR_BASE/results/${method}/${RUN_ID}_score.json"

        if [ -d "$WORKDIR" ]; then
            python "$SCRIPT_DIR/score_candidate.py" "$WORKDIR" --output "$SCORE_FILE"
        fi
    done
done

# Run summarization
echo ""
echo "Generating summary..."
python "$SCRIPT_DIR/summarize.py" "$WORKDIR_BASE/results" --output "$WORKDIR_BASE/results/summary.md"

echo ""
echo "Benchmark complete!"
echo "Results saved to: $WORKDIR_BASE/results/summary.md"