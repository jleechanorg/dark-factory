"""Dark Factory CLI entry point."""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import shlex
import time
import sys
import traceback

from .engine import run
from .handlers import Context
from .panic_hook import PANIC_EXIT_CODE
from .parser import Graph, parse, validate_pipeline
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
                "returncode": str(PANIC_EXIT_CODE),
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


def _missing_holdout_feature_nodes(graph: Graph, state: dict[str, str]) -> list[str]:
    """Return holdout nodes that cannot resolve a feature before execution."""
    missing: list[str] = []
    for name, node in graph.nodes.items():
        if str(node.attrs.get("type") or "") != "holdout_eval":
            continue
        if str(node.attrs.get("allow_missing_feature") or "").lower() == "true":
            continue
        feature = str(node.attrs.get("feature") or "").strip()
        if not feature:
            if not str(state.get("feature") or "").strip():
                missing.append(name)
            continue
        unresolved = False
        resolved = feature
        while "${state." in resolved:
            start = resolved.find("${state.")
            end = resolved.find("}", start)
            if end == -1:
                unresolved = True
                break
            key = resolved[start + len("${state."):end]
            value = str(state.get(key) or "")
            if not value:
                unresolved = True
            resolved = resolved[:start] + value + resolved[end + 1:]
        if unresolved or not resolved.strip():
            missing.append(name)
    return missing


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
                "event": "crash",
                "run_id": run_id or "",
                "exit_code": str(PANIC_EXIT_CODE),
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


def _finalize_evidence_bundle_path(
    args: argparse.Namespace,
    ctx: Context,
    *,
    explicit_evidence_bundle: bool,
    evidence_staging_dir: pathlib.Path | None,
) -> None:
    if (
        args.evidence_bundle is not None
        and not explicit_evidence_bundle
        and evidence_staging_dir is not None
        and ctx.run_id is not None
    ):
        old_cxdb = args.cxdb
        old_events = args.events
        final_evidence_bundle = args.workdir / "evidence" / ctx.run_id
        if pathlib.Path(args.evidence_bundle) != final_evidence_bundle:
            if final_evidence_bundle.exists():
                shutil.rmtree(final_evidence_bundle)
            args.evidence_bundle.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(args.evidence_bundle), str(final_evidence_bundle))
            args.evidence_bundle = final_evidence_bundle
        if old_cxdb is not None and pathlib.Path(old_cxdb).parent == evidence_staging_dir:
            args.cxdb = final_evidence_bundle / pathlib.Path(old_cxdb).name
            ctx.cxdb_path = args.cxdb
        if old_events is not None and pathlib.Path(old_events).parent == evidence_staging_dir:
            args.events = final_evidence_bundle / "events.jsonl"
            ctx.event_log_path = args.events


def _write_evidence_bundle(
    *,
    args: argparse.Namespace,
    ctx: Context,
    pipeline_path: pathlib.Path,
    graph,
    command: str,
) -> None:
    if args.evidence_bundle is None or ctx.run_id is None:
        return
    from .evidence import write_bundle

    write_bundle(
        bundle_dir=args.evidence_bundle,
        cxdb_path=args.cxdb,
        run_id=ctx.run_id,
        pipeline_path=pipeline_path,
        graph=graph,
        workdir=args.workdir,
        command=command,
        event_log_path=args.events,
    )


def main(argv: list[str] | None = None) -> int:
    args_list = list(argv) if argv is not None else list(sys.argv[1:])
    if args_list and args_list[0] == "resume":
        args_list[0] = "--resume"
    argv = args_list
    command = shlex.join(["dark-factory", *argv])

    args = None
    ctx = None
    pipeline_path = None
    graph = None
    explicit_evidence_bundle = False
    evidence_staging_dir = None
    try:
        p = argparse.ArgumentParser(prog="dark-factory")
        p.add_argument("--pipeline", type=pathlib.Path)
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
                "per-step files, single-run CXDB extract). Defaults to evidence/<run-id>."
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
        if args.resume:
            val = str(args.resume)
            import re
            is_12_hex = bool(re.match(r'^[0-9a-fA-F]{12}$', val))
            manifest_file = pathlib.Path.home() / ".dark-factory" / "runs" / val / "manifest.json"
            if is_12_hex or manifest_file.exists():
                if manifest_file.exists():
                    try:
                        manifest = json.loads(manifest_file.read_text(encoding="utf-8"))
                        args.pipeline = pathlib.Path(manifest["pipeline"])
                        args.goal = manifest.get("goal")
                        args.backend = manifest.get("backend", "echo")
                        if manifest.get("workdir"):
                            args.workdir = pathlib.Path(manifest["workdir"])
                        state_dict = manifest.get("state", {})
                        args.state = [f"{k}={v}" for k, v in state_dict.items()]
                    except Exception as exc:
                        sys.stderr.write(f"Error loading manifest from {manifest_file}: {exc}\n")
                        return 1
                checkpoint_path = pathlib.Path.home() / ".dark-factory" / "runs" / val / "checkpoint.json"
                args.resume = checkpoint_path
                args.checkpoint = checkpoint_path

        if not args.pipeline:
            p.error("the following arguments are required: --pipeline")

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

        graph = parse(pipeline_path)
        initial_state: dict[str, str] = {}
        for kv in args.state:
            if "=" not in kv:
                p.error(f"--state requires KEY=VALUE format, got: {kv!r}")
            k, v = kv.split("=", 1)
            initial_state[k] = v
        initial_state.setdefault("_df_shadow_codex_review", "true")
        if args.feature:
            initial_state["feature"] = args.feature
        missing_feature_nodes = _missing_holdout_feature_nodes(graph, initial_state)
        if missing_feature_nodes:
            p.error(
                "holdout_eval requires feature metadata before execution; "
                f"missing for node(s): {', '.join(missing_feature_nodes)}. "
                'Pass --feature <feature>, pass --state feature=<feature>, set node feature="<holdout-feature>", '
                'or mark allow_missing_feature="true" for intentional opt-outs.'
            )

        explicit_evidence_bundle = args.evidence_bundle is not None
        evidence_staging_dir = None
        if args.evidence_bundle is None:
            evidence_root = args.workdir / "evidence"
            evidence_staging_dir = evidence_root / f"_pending-{int(time.time() * 1000)}"
            args.evidence_bundle = evidence_staging_dir
        if args.evidence_bundle is not None:
            args.evidence_bundle.mkdir(parents=True, exist_ok=True)

        if args.evidence_bundle is not None and args.events is None:
            args.events = args.evidence_bundle / "events.jsonl"

        if args.evidence_bundle is not None and args.cxdb is None:
            # The bundle's `cxdb-<run_id>.sqlite` extract needs a source CXDB —
            # auto-provision one in the bundle dir if none was supplied so the
            # CLI is forgiving for ad-hoc smoke runs.
            args.cxdb = args.evidence_bundle / "_run.sqlite"

        perf_log_root = None if args.no_perf_log else args.perf_log_dir
        ctx = Context(
            goal=args.goal,
            workdir=args.workdir,
            backend=args.backend,
            cxdb_path=args.cxdb,
            event_log_path=args.events,
            perf_log_root=perf_log_root,
        )
        ctx.state.update(initial_state)
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

        _finalize_evidence_bundle_path(
            args,
            ctx,
            explicit_evidence_bundle=explicit_evidence_bundle,
            evidence_staging_dir=evidence_staging_dir,
        )
        _write_evidence_bundle(
            args=args,
            ctx=ctx,
            pipeline_path=pipeline_path,
            graph=graph,
            command=command,
        )

        summary = {
            "pipeline": graph.name,
            "goal": args.goal,
            "run_id": ctx.run_id,
            "command": command,
            "run_dir": (
                str(pathlib.Path.home() / ".dark-factory" / "runs" / ctx.run_id)
                if ctx.run_id
                else None
            ),
            "inputs_dir": (
                str(pathlib.Path.home() / ".dark-factory" / "runs" / ctx.run_id / "inputs")
                if ctx.run_id
                else None
            ),
            "transcripts_dir": (
                str(pathlib.Path.home() / ".dark-factory" / "runs" / ctx.run_id / "transcripts")
                if ctx.run_id
                else None
            ),
            "events": str(ctx.event_log_path) if ctx.event_log_path else None,
            "evidence_bundle": str(args.evidence_bundle) if args.evidence_bundle else None,
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
        if (
            args is not None
            and ctx is not None
            and ctx.run_id is not None
            and pipeline_path is not None
            and graph is not None
        ):
            try:
                old_artifact = payload.get("panic_artifact")
                _finalize_evidence_bundle_path(
                    args,
                    ctx,
                    explicit_evidence_bundle=explicit_evidence_bundle,
                    evidence_staging_dir=evidence_staging_dir,
                )
                if old_artifact and evidence_staging_dir is not None:
                    try:
                        rel = pathlib.Path(old_artifact).relative_to(evidence_staging_dir)
                        payload["panic_artifact"] = str(pathlib.Path(args.evidence_bundle) / rel)
                    except ValueError:
                        pass
                _write_evidence_bundle(
                    args=args,
                    ctx=ctx,
                    pipeline_path=pipeline_path,
                    graph=graph,
                    command=command,
                )
                payload["evidence_bundle"] = str(args.evidence_bundle)
                payload["evidence_bundle_written"] = "true"
            except Exception as bundle_exc:
                payload["evidence_bundle_written"] = "false"
                payload["evidence_bundle_error"] = f"{type(bundle_exc).__name__}: {bundle_exc}"
        print(json.dumps(payload, sort_keys=True, indent=2))
        return PANIC_EXIT_CODE


if __name__ == "__main__":
    sys.exit(main())
