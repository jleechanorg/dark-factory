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
        choices=["echo", "claude", "codex", "ao", "agy", "claudew"],
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
    p.add_argument("--resume", type=pathlib.Path, default=None, help="Resume from a prior checkpoint file.")
    p.add_argument("--max-steps", type=int, default=100)
    p.add_argument("--feature", default=None, help="feature name for holdout eval")
    p.add_argument("--ao-session", default=None, help="Reuse existing AO session (skip spawn)")
    p.add_argument("--ao-worktree", default=None, help="AO worktree path to use for holdout eval")
    p.add_argument(
        "--cxdb",
        type=pathlib.Path,
        default=None,
        help="Path to CXDB SQLite file; records every step for Healer consumption.",
    )
    p.add_argument(
        "--evidence-bundle",
        type=pathlib.Path,
        default=None,
        help=(
            "Directory to materialise a per-run evidence bundle (manifest, "
            "per-step files, single-run CXDB extract). Requires --cxdb."
        ),
    )
    args = p.parse_args(argv)

    if args.evidence_bundle is not None and args.cxdb is None:
        # The bundle's `cxdb-<run_id>.sqlite` extract needs a source CXDB —
        # auto-provision one in the bundle dir if none was supplied so the
        # CLI is forgiving for ad-hoc smoke runs.
        args.evidence_bundle.mkdir(parents=True, exist_ok=True)
        args.cxdb = args.evidence_bundle / "_run.sqlite"

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
    if args.ao_session:
        ctx.state["ao.session"] = args.ao_session
    if args.ao_worktree:
        ctx.state["ao.worktree"] = str(pathlib.Path(args.ao_worktree).expanduser().resolve())

    history = run(
        graph,
        ctx,
        checkpoint=args.checkpoint,
        resume=args.resume,
        max_steps=args.max_steps,
    )

    if args.evidence_bundle is not None and ctx.run_id is not None:
        # Lazy import so the CLI module stays cheap to load.
        from .evidence import write_bundle

        write_bundle(
            bundle_dir=args.evidence_bundle,
            cxdb_path=args.cxdb,
            run_id=ctx.run_id,
            pipeline_path=args.pipeline,
            graph=graph,
            workdir=args.workdir,
        )

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
