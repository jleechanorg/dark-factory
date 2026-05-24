#!/usr/bin/env bash
# Deterministic matrix smoke for both spec-review graphs with mocked codergen/tool handlers.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(dirname "$SCRIPT_DIR")"
PROJECT_ROOT="$(cd "$BENCHMARK_DIR/../.." && pwd)"

OUTPUT_DIR="${1:-$BENCHMARK_DIR/results/deterministic-matrix}"
mkdir -p "$OUTPUT_DIR"

cd "$PROJECT_ROOT"

.venv/bin/python - "$OUTPUT_DIR" <<'PY'
from __future__ import annotations

import json
import pathlib
import sys

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse


OUT = pathlib.Path(sys.argv[1]).resolve()
ROOT = pathlib.Path.cwd()

METHODS = {
    "review-slim": ROOT / "benchmarks/attractor-spec-review/pipelines/review_slim.dot",
    "review-full": ROOT / "benchmarks/attractor-spec-review/pipelines/review_full.dot",
}


def fake_codergen(node, ctx):
    return Result(
        outcome="success",
        output=f"mock codergen node={node.name} prompt={node.attrs.get('prompt', 'unknown')}",
        metadata={"mock_node": node.name, "mock_handler": "codergen"},
    )


def fake_tool(node, ctx):
    return Result(
        outcome="success",
        output=f"mock tool node={node.name} command={node.attrs.get('command', '')}",
        metadata={"mock_node": node.name, "mock_handler": "tool"},
    )


TYPE_REGISTRY["codergen"] = fake_codergen
TYPE_REGISTRY["tool"] = fake_tool

summary = {"benchmark": "attractor-spec-review", "methods": {}}

for method, pipeline_path in METHODS.items():
    graph = parse(pipeline_path)
    workdir = OUT / method
    workdir.mkdir(parents=True, exist_ok=True)
    ctx = Context(
        goal=f"deterministic smoke for {method}",
        workdir=workdir,
        backend="echo",
    )
    ctx.state["feature"] = "attractor-spec-review"
    history = run(graph, ctx, checkpoint=workdir / "checkpoint.json", max_steps=40)
    status = "pass" if history and history[-1].outcome == "success" else "fail"
    result = {
        "method": method,
        "pipeline": str(pipeline_path.relative_to(ROOT)),
        "status": status,
        "final_outcome": history[-1].outcome if history else "empty",
        "steps": [{"node": item.node, "outcome": item.outcome} for item in history],
    }
    summary["methods"][method] = result
    (workdir / "run.json").write_text(json.dumps(result, indent=2))

summary["status"] = (
    "pass" if all(item["status"] == "pass" for item in summary["methods"].values()) else "fail"
)
summary_path = OUT / "summary.json"
summary_path.write_text(json.dumps(summary, indent=2))

print(json.dumps(summary, indent=2))
raise SystemExit(0 if summary["status"] == "pass" else 1)
PY
