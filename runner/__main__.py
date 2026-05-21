"""Dark Factory CLI entry point."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

from .engine import run
from .handlers import Context
from .parser import parse


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="dark-factory")
    p.add_argument("--pipeline", required=True, type=pathlib.Path)
    p.add_argument("--goal", required=True)
    p.add_argument("--workdir", type=pathlib.Path, default=pathlib.Path.cwd())
    p.add_argument(
        "--backend",
        choices=["echo", "claude", "codex", "ao"],
        default="echo",
        help="LLM backend for codergen nodes",
    )
    p.add_argument(
        "--ao-project",
        default=None,
        help="AO project ID (required when --backend ao). Stored in state['ao.project'].",
    )
    p.add_argument(
        "--ao-agent",
        default="claude-code",
        help="AO agent plugin to use when --backend ao (default: claude-code).",
    )
    p.add_argument("--checkpoint", type=pathlib.Path, default=None)
    p.add_argument("--max-steps", type=int, default=100)
    p.add_argument("--feature", default=None, help="feature name for holdout eval")
    p.add_argument(
        "--cxdb",
        type=pathlib.Path,
        default=None,
        help="Path to CXDB SQLite file; records every step for Healer consumption.",
    )
    args = p.parse_args(argv)

    graph = parse(args.pipeline)
    ctx = Context(
        goal=args.goal,
        workdir=args.workdir,
        backend=args.backend,
        cxdb_path=args.cxdb,
    )
    if args.feature:
        ctx.state["feature"] = args.feature
    if args.backend == "ao":
        if not args.ao_project:
            p.error("--backend ao requires --ao-project")
        ctx.state["ao.project"] = args.ao_project
        ctx.state["ao.agent"] = args.ao_agent

    history = run(graph, ctx, checkpoint=args.checkpoint, max_steps=args.max_steps)

    summary = {
        "pipeline": graph.name,
        "goal": args.goal,
        "steps": len(history),
        "final_outcome": history[-1].outcome if history else "empty",
        "trace": [
            {"node": r.node, "outcome": r.outcome, "preview": r.output_preview[:120]}
            for r in history
        ],
    }
    print(json.dumps(summary, indent=2))
    return 0 if history and history[-1].outcome == "success" else 1


if __name__ == "__main__":
    sys.exit(main())
