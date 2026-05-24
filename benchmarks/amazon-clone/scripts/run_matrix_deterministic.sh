#!/usr/bin/env bash
# Deterministically exercise every Amazon benchmark DOT pipeline with mock
# codergen/holdout/tool behavior. This validates orchestration shape only; it
# does not run real coding agents or the sealed evaluator.

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


ROOT = pathlib.Path.cwd()
OUT = pathlib.Path(sys.argv[1]).resolve()

methods = {
    "dark-factory": ROOT / "benchmarks/amazon-clone/pipelines/dark_factory.dot",
    "df-slim": ROOT / "benchmarks/amazon-clone/pipelines/slim.dot",
    "kilroy": ROOT / "benchmarks/amazon-clone/pipelines/kilroy.dot",
    "mammoth": ROOT / "benchmarks/amazon-clone/pipelines/mammoth.dot",
    "tracker": ROOT / "benchmarks/amazon-clone/pipelines/tracker.dot",
    "smasher": ROOT / "benchmarks/amazon-clone/pipelines/smasher.dot",
}


def fake_holdout(node, ctx):
    return Result(
        outcome="success",
        output=json.dumps(
            {
                "benchmark": "amazon-clone",
                "sealed": True,
                "verdict": "pass",
                "passed": 1,
                "total": 1,
            }
        ),
        metadata={"sealed": "true", "mock": "true"},
    )


TYPE_REGISTRY["holdout_eval"] = fake_holdout

results = {}
for method, pipeline in methods.items():
    graph = parse(pipeline)
    workdir = OUT / method
    workdir.mkdir(parents=True, exist_ok=True)
    ctx = Context(
        goal="deterministic Amazon benchmark matrix smoke",
        workdir=workdir,
        backend="echo",
    )
    ctx.state["feature"] = "amazon-clone"
    ctx.state["amazon.public_command"] = "python -c 'print(\"public acceptance mock passed\")'"
    ctx.state["amazon.holdout_command"] = "python -c 'print(\"redacted holdout mock passed\")'"
    history = run(graph, ctx, checkpoint=workdir / "checkpoint.json", max_steps=50)
    result = {
        "method": method,
        "pipeline": str(pipeline.relative_to(ROOT)),
        "status": "pass" if history and history[-1].outcome == "success" else "fail",
        "final_outcome": history[-1].outcome if history else "empty",
        "steps": [{"node": item.node, "outcome": item.outcome} for item in history],
        "scope": "deterministic_orchestration_smoke",
    }
    (workdir / "run.json").write_text(json.dumps(result, indent=2))
    results[method] = result

summary = {
    "benchmark": "amazon-clone",
    "scope": "deterministic_orchestration_smoke",
    "methods": results,
    "status": "pass" if all(r["status"] == "pass" for r in results.values()) else "fail",
}
(OUT / "summary.json").write_text(json.dumps(summary, indent=2))
print(json.dumps(summary, indent=2))
raise SystemExit(0 if summary["status"] == "pass" else 1)
PY
