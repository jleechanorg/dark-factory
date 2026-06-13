"""Dark Factory CLI entry point."""

from __future__ import annotations

import argparse
import json
import pathlib
import time
import sys
import traceback

from . import _agy_safe
from .engine import run
from .handlers import Context
from .parser import parse, validate_pipeline
from .paths import resolve_factory_path, resolve_pipeline_path

_PANIC_DIR = pathlib.Path.home() / ".dark-factory" / "panics"


def _append_event(path: pathlib.Path, payload: dict[str, str]) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.open("a", encoding="utf-8").write(json.dumps(payload, sort_keys=True) + "\n")
    except Exception:
        pass


def _record_cxdb_panic(
    run_id: str,
    cxdb_path: pathlib.Path,
    traceback_text: str,
) -> tuple[bool, str | None, str | None]:
    from .cxdb import CXDB

    cxdb = None
    try:
        cxdb = CXDB(cxdb_path)
        row = cxdb._conn.execute(
            "SELECT 1 FROM runs WHERE run_id = ?",
            (run_id,),
        ).fetchone()
        if row is None:
            return False, None, None
        seq_row = cxdb._conn.execute(
            "SELECT COALESCE(MAX(seq), -1) FROM steps WHERE run_id = ?",
            (run_id,),
        ).fetchone()
        seq = int(seq_row[0]) + 1 if seq_row else 0
        cxdb.record_step(
            run_id=run_id,
            seq=seq,
            node="panic",
            outcome="error",
            ts=time.time(),
            output=traceback_text,
            metadata={
                "event": "panic",
                "returncode": "128",
            },
        )
        cxdb.end_run(run_id=run_id, final="error")
        return True, str(seq), None
    except Exception as exc:
        return False, None, f"{type(exc).__name__}: {exc}"
    finally:
        if cxdb is not None:
            try:
                cxdb.close()
            except Exception:
                pass


def _handle_panic(
    exc: BaseException,
    *,
    args: argparse.Namespace | None,
    ctx: Context | None,
) -> dict[str, str | None]:
    traceback_text = traceback.format_exc()
    run_id = getattr(ctx, "run_id", None)
    event_log_path = getattr(ctx, "event_log_path", None)
    cxdb_path = getattr(ctx, "cxdb_path", None)
    if args is not None and event_log_path is None and getattr(args, "events", None) is not None:
        event_log_path = args.events

    payload: dict[str, str | None] = {
        "status": "panic",
        "error": f"{type(exc).__name__}: {exc}",
        "traceback": traceback_text,
        "command": " ".join(sys.argv),
        "run_id": run_id,
        "pipeline": str(getattr(args, "pipeline", "")) if args is not None else None,
        "goal": getattr(args, "goal", None),
        "events": str(event_log_path) if event_log_path else None,
        "cxdb_path": str(cxdb_path) if cxdb_path else None,
    }

    panic_root = _PANIC_DIR
    if args is not None and getattr(args, "evidence_bundle", None) is not None:
        panic_root = pathlib.Path(args.evidence_bundle) / "panics"
    panic_root.mkdir(parents=True, exist_ok=True)
    panic_file = panic_root / f"panic-{(run_id or 'no-run')}-{int(time.time())}.json"
    try:
        panic_file.write_text(json.dumps(payload, sort_keys=True, indent=2))
    except Exception as exc2:
        payload["panic_artifact_write_error"] = f"{type(exc2).__name__}: {exc2}"
    payload["panic_artifact"] = str(panic_file)

    if payload["events"]:
        _append_event(
            pathlib.Path(payload["events"]),
            {
                "event": "panic",
                "run_id": run_id or "",
                "exit_code": "128",
                "panic_artifact": str(panic_file),
                "error": f"{type(exc).__name__}: {exc}",
            },
        )

    if run_id and cxdb_path:
        recorded, seq, cxdb_err = _record_cxdb_panic(run_id, pathlib.Path(cxdb_path), traceback_text)
        payload["cxdb_recorded"] = "true" if recorded else "false"
        payload["cxdb_seq"] = seq
        if cxdb_err:
            payload["cxdb_error"] = cxdb_err
    else:
        payload["cxdb_recorded"] = "false"

    return payload


def main(argv: list[str] | None = None) -> int:
    _agy_safe.install()
    args = None
    ctx = None
    try:
        p = argparse.ArgumentParser(prog="dark-factory")
        p.add_argument("--pipeline", required=True, type=pathlib.Path)
        p.add_argument("--goal")
        p.add_argument("--workdir", type=pathlib.Path, default=pathlib.Path.cwd())
        p.add_argument("--preflight", action="store_true", help="Validate pipeline and emit diagnostics, then exit.")
        p.add_argument(
            "--backend",
            choices=["echo", "claude", "codex", "ao", "agy"],
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
        p.add_argument(
            "--events",
            type=pathlib.Path,
            default=None,
            help="Optional JSONL event log path for structured run events.",
        )
        p.add_argument(
            "--perf-log-dir",
            type=pathlib.Path,
            default=pathlib.Path.home() / "Library" / "Logs" / "dark-factory",
            help="Root directory for repo/branch performance logs (default: ~/Library/Logs/dark-factory).",
        )
        p.add_argument(
            "--no-perf-log",
            action="store_true",
            help="Disable performance logging under --perf-log-dir.",
        )
        p.add_argument(
            "--state",
            action="append",
            default=[],
            metavar="KEY=VALUE",
            help="Pre-seed ctx.state; repeatable. E.g. --state slim.test_command=true",
        )
        args = p.parse_args(argv)
        # --workdir defaults to cwd in the argparse setup above; the resolver
        # uses it to also look up <workdir>/dark-factory/pipelines/<name> for
        # bare-filename --pipeline values (target-repo subdir convention).
        pipeline_path = resolve_pipeline_path(args.pipeline, workdir=args.workdir)

        if args.preflight:
            graph, diagnostics = validate_pipeline(pipeline_path)
            payload = {
                "pipeline": str(pipeline_path),
                "pipeline_name": graph.name if graph else pipeline_path.stem,
                "goal": graph.goal if graph else "",
                "diagnostics": diagnostics,
            }
            payload["status"] = "fail" if any(item.get("severity") == "error" for item in diagnostics) else "pass"
            print(json.dumps(payload, sort_keys=True, indent=2))
            return 1 if payload["status"] == "fail" else 0

        if not args.goal:
            p.error("--goal is required unless --preflight is set")

        if args.evidence_bundle is not None and args.events is None:
            args.events = args.evidence_bundle / "events.jsonl"

        if args.evidence_bundle is not None and args.cxdb is None:
            # The bundle's `cxdb-<run_id>.sqlite` extract needs a source CXDB —
            # auto-provision one in the bundle dir if none was supplied so the
            # CLI is forgiving for ad-hoc smoke runs.
            args.evidence_bundle.mkdir(parents=True, exist_ok=True)
            args.cxdb = args.evidence_bundle / "_run.sqlite"

        graph = parse(pipeline_path)
        perf_log_root = None if args.no_perf_log else args.perf_log_dir
        ctx = Context(
            goal=args.goal,
            workdir=args.workdir,
            backend=args.backend,
            cxdb_path=args.cxdb,
            event_log_path=args.events,
            perf_log_root=perf_log_root,
        )
        for kv in args.state:
            if "=" not in kv:
                p.error(f"--state requires KEY=VALUE format, got: {kv!r}")
            k, v = kv.split("=", 1)
            ctx.state[k] = v
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
                pipeline_path=pipeline_path,
                graph=graph,
                workdir=args.workdir,
                event_log_path=args.events,
            )

        summary = {
            "pipeline": graph.name,
            "goal": args.goal,
            "run_id": ctx.run_id,
            "events": str(ctx.event_log_path) if ctx.event_log_path else None,
            "steps": len(history),
            "final_outcome": history[-1].outcome if history else "empty",
            "trace": [
                {
                    "node": r.node,
                    "outcome": r.outcome,
                    "preview": r.output_preview[:120],
                    # Reviewer-gate audit fields — without these every gate
                    # failure needs transcript forensics to attribute infra
                    # vs a real FAIL verdict.
                    **{
                        k: v
                        for k, v in (r.metadata or {}).items()
                        if k in (
                            "verdict", "reviewer_backend", "fallback_used",
                            "fallback_from", "head_sha_status", "timed_out",
                        )
                    },
                }
                for r in history
            ],
        }
        perf_run = getattr(ctx, "perf_run", None)
        if perf_run is not None:
            summary["perf_log"] = {
                "jsonl": str(perf_run.jsonl_path),
                "log": str(perf_run.log_path),
                "repo": perf_run.git_ctx.repo_slug,
                "branch": perf_run.git_ctx.branch_slug,
            }
        elif perf_log_root is not None and ctx.git_ctx is not None:
            summary["perf_log"] = {
                "repo": ctx.git_ctx.repo_slug,
                "branch": ctx.git_ctx.branch_slug,
            }
        print(json.dumps(summary, indent=2))
        return 0 if history and history[-1].outcome == "success" else 1
    except Exception:
        payload = _handle_panic(sys.exc_info()[1], args=args, ctx=ctx)
        print(json.dumps(payload, sort_keys=True, indent=2))
        return 128


if __name__ == "__main__":
    sys.exit(main())
