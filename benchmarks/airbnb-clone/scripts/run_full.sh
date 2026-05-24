#!/bin/bash
# Full run — AO backend with Claude Sonnet override, real holdout evaluation.
# Usage: ./scripts/run_full.sh [--sprint 1|2|3|all] [--ao-agent <agent>] [--workdir <dir>]
#
# Prerequisites:
#   DARK_FACTORY_HOLDOUTS  path to sealed holdouts repo (required)
#   AO_CONFIG_PATH         optional: path to custom AO config (for model override)
#
# Model override (Sonnet without editing agent-orchestrator.yaml):
#   cp ~/.hermes/agent-orchestrator.yaml /tmp/ao-airbnb.yaml
#   # Edit /tmp/ao-airbnb.yaml: under agentConfig, set model: claude-sonnet-4-6
#   AO_CONFIG_PATH=/tmp/ao-airbnb.yaml ./scripts/run_full.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(cd "$BENCHMARK_DIR/../.." && pwd)"
PYTHON_BIN="python"
if [ -x "$PROJECT_ROOT/.venv/bin/python" ]; then
    PYTHON_BIN="$PROJECT_ROOT/.venv/bin/python"
fi

: "${DARK_FACTORY_HOLDOUTS:?DARK_FACTORY_HOLDOUTS must point to the sealed holdouts repo}"

# AO_CONFIG_PATH: use isolated single-project config to avoid duplicate-basename
# validation error when 'dark-factory' project exists alongside other projects.
if [ -z "${AO_CONFIG_PATH:-}" ]; then
  _AO_TMP_CONFIG="/tmp/ao-dark-factory.yaml"
  python3 -c "
import yaml, pathlib, os, sys
src = pathlib.Path.home() / '.hermes/agent-orchestrator.yaml'
with open(src) as f:
    config = yaml.safe_load(f)
projects = config.get('projects', {})
df = projects.get('dark-factory')
if not df:
    print('ERROR: dark-factory project not found in ~/.hermes/agent-orchestrator.yaml', file=sys.stderr)
    sys.exit(1)
config['projects'] = {'dark-factory': df}
with open('${_AO_TMP_CONFIG}', 'w') as f:
    yaml.dump(config, f, default_flow_style=False, allow_unicode=True)
"
  export AO_CONFIG_PATH="${_AO_TMP_CONFIG}"
  echo "AO_CONFIG_PATH auto-set to: ${_AO_TMP_CONFIG}"
fi

# Parse args
SPRINT="all"
AO_AGENT="${AO_AGENT:-claude-code}"
WORKDIR="${WORKDIR_BASE:-$HOME/benchmark-runs/airbnb-clone}"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RUN_ID="run_${TIMESTAMP}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --sprint) SPRINT="$2"; shift 2 ;;
        --ao-agent) AO_AGENT="$2"; shift 2 ;;
        --workdir) WORKDIR="$2"; shift 2 ;;
        *) echo "Unknown arg: $1"; exit 1 ;;
    esac
done

RESULTS_DIR="$WORKDIR/$RUN_ID"
mkdir -p "$RESULTS_DIR"
LOG_FILE="$RESULTS_DIR/run.log"
CXDB="$RESULTS_DIR/cxdb.sqlite"

declare -A PIPELINES=(
    ["1"]="benchmarks/airbnb-clone/pipelines/sprint-1-data.dot"
    ["2"]="benchmarks/airbnb-clone/pipelines/sprint-2-backend.dot"
    ["3"]="benchmarks/airbnb-clone/pipelines/sprint-3-frontend.dot"
    ["all"]="benchmarks/airbnb-clone/pipelines/airbnb-clone.dot"
)

if [ -z "${PIPELINES[$SPRINT]:-}" ]; then
    echo "ERROR: --sprint must be 1, 2, 3, or all (got: $SPRINT)"
    exit 1
fi

PIPELINE="${PIPELINES[$SPRINT]}"

# Feature name maps sprint → feature flag
declare -A FEATURES=(
    ["1"]="airbnb-clone-sprint-1"
    ["2"]="airbnb-clone-sprint-2"
    ["3"]="airbnb-clone-sprint-3"
    ["all"]="airbnb-clone"
)
FEATURE="${FEATURES[$SPRINT]}"

echo "============================================"
echo "Airbnb Clone — Full Run"
echo "============================================"
echo "Sprint: $SPRINT"
echo "Pipeline: $PIPELINE"
echo "Agent: $AO_AGENT"
echo "Feature: $FEATURE"
echo "Results: $RESULTS_DIR"
echo "Started: $(date)"
if [ -n "${AO_CONFIG_PATH:-}" ]; then
    echo "AO config override: $AO_CONFIG_PATH"
fi
echo ""

# Check Firebase emulators are running (Sprint 1–3 need them for holdout eval)
if ! curl -sf "http://localhost:8080" >/dev/null 2>&1; then
    echo "WARNING: Firestore emulator not detected at :8080"
    echo "  Start with: firebase emulators:start --project airbnb-clone-dev"
    echo "  Continuing anyway (evaluator will handle the failure)."
fi

cd "$PROJECT_ROOT"

set +e
"$PYTHON_BIN" -m runner \
    --pipeline "$PIPELINE" \
    --goal "Implement a full Airbnb clone with Firebase emulator backend per spec.md" \
    --backend ao \
    --ao-agent "$AO_AGENT" \
    --ao-project "dark-factory" \
    --feature "$FEATURE" \
    --cxdb "$CXDB" \
    --evidence-bundle "$RESULTS_DIR/evidence" \
    2>&1 | tee "$LOG_FILE"
EXIT_CODE=$?
set -e

echo ""
echo "============================================"
echo "Run completed"
echo "Exit code: $EXIT_CODE"
echo "Finished: $(date)"
echo ""
echo "Artifacts:"
echo "  Log:      $LOG_FILE"
echo "  CXDB:     $CXDB"
echo "  Evidence: $RESULTS_DIR/evidence/"
echo "============================================"

# Write result metadata
cat > "$RESULTS_DIR/result.json" <<EOF
{
  "run_id": "$RUN_ID",
  "sprint": "$SPRINT",
  "pipeline": "$PIPELINE",
  "ao_agent": "$AO_AGENT",
  "feature": "$FEATURE",
  "timestamp": "$TIMESTAMP",
  "exit_code": $EXIT_CODE,
  "status": "$([ $EXIT_CODE -eq 0 ] && echo success || echo failed)",
  "artifacts": {
    "log": "$LOG_FILE",
    "cxdb": "$CXDB",
    "evidence": "$RESULTS_DIR/evidence/"
  }
}
EOF

echo "Result metadata: $RESULTS_DIR/result.json"

if [ $EXIT_CODE -ne 0 ]; then
    echo ""
    echo "Run failed. Check CXDB for failure clusters:"
    echo "  python -m runner.healer --cxdb $CXDB"
fi

exit $EXIT_CODE
