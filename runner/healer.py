"""Healer — read CXDB, cluster failures, emit a diagnosis report.

Clusters: rows in CXDB.steps with terminal failure outcomes, grouped by
(node, outcome, output_hash). Each cluster gets a prescription template
that suggests where to look (prompt template, holdout scenario, node config).
"""

from __future__ import annotations

import argparse
import pathlib
import sys
from dataclasses import dataclass
from typing import Optional

from . import _agy_safe
from .cxdb import CXDB


@dataclass
class Cluster:
    node: str
    outcome: str
    output_hash: str
    hits: int
    sample: str
    run_ids: list[str]
    # Aggregate cost columns (orch-qhez). Each may be None when no member row
    # of the cluster recorded the corresponding metric — distinct from zero.
    total_tokens: Optional[int] = None
    total_cost_usd: Optional[float] = None
    total_wall_ms: Optional[int] = None

    # Heuristic over the Healer's own node-name namespace (not user/model
    # input), so prefix matching here is internal data routing, not ZFC.
    @property
    def is_gate(self) -> bool:
        return self.node.startswith("gate_") or self.node in {"holdout", "validate"}

    def prescription(self, backend: str = "echo", cxdb_path: Optional[pathlib.Path] = None) -> str:
        return diagnose_cluster(self, backend, cxdb_path)


def diagnose_cluster(cluster: Cluster, backend: str = "echo", cxdb_path: Optional[pathlib.Path] = None) -> str:
    # 1. Fetch context state (goal, pipeline, metadata_json) from CXDB if cxdb_path is provided
    goal = "unknown"
    pipeline = "unknown"
    metadata_json = "{}"

    if cxdb_path is not None:
        db = CXDB(cxdb_path)
        try:
            cur = db._conn.cursor()
            if cluster.run_ids:
                cur.execute("SELECT goal, pipeline FROM runs WHERE run_id = ?", (cluster.run_ids[0],))
                row = cur.fetchone()
                if row:
                    goal, pipeline = row[0], row[1]

            cur.execute(
                "SELECT metadata_json FROM steps WHERE node = ? AND outcome = ? AND output_hash = ? LIMIT 1",
                (cluster.node, cluster.outcome, cluster.output_hash)
            )
            row = cur.fetchone()
            if row:
                metadata_json = row[0]
        except Exception:
            pass
        finally:
            db.close()

    # 2. Call the active LLM backend to analyze the error output and context state
    if backend == "echo":
        return f"[mock echo prescription] Node '{cluster.node}' failed with '{cluster.outcome}' in pipeline '{pipeline}' (goal: '{goal}'). Metadata: {metadata_json}."

    elif backend == "claude":
        # Build the prompt instructing the model to analyze the root cause and generate a prescription
        prompt = f"""You are the Dark Factory Healer, an expert AI debugging agent.
An automated pipeline run encountered a terminal failure.
Your task is to analyze the error output and context state, diagnose the root cause, and generate a highly specific, evidence-backed prescription to fix the failure.

Failed Node Details:
- Node Name: {cluster.node}
- Failure Outcome: {cluster.outcome}
- Occurrences (Hits): {cluster.hits}

Pipeline Context State:
- Pipeline Name: {pipeline}
- Run Goal: {goal}
- Step Metadata: {metadata_json}

Terminal stdout/stderr sample:
```
{cluster.sample}
```

Please analyze the root cause of this failure and generate a highly specific, evidence-backed prescription advising where to look (e.g. prompt template, holdout scenario, node config) and how to resolve it. Do not use generic placeholders.
Your output must consist entirely of the prescription (no preamble, no conversational filler)."""

        from .handlers import _sandboxed_args, _get_claude_executable, _sanitized_env
        import subprocess

        args = _sandboxed_args([_get_claude_executable(), "--print", "--dangerously-skip-permissions", "--setting-sources", "", prompt])
        if args is None:
            return f"Error: sandbox-exec unavailable. Failed to diagnose {cluster.node}."

        try:
            proc = subprocess.run(
                args,
                capture_output=True,
                text=True,
                timeout=1800,  # 30 min timeout for complex tasks
                check=False,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
                env=_sanitized_env(),
            )
            if proc.returncode == 0:
                return proc.stdout.strip()
            else:
                return f"Error: LLM backend failed (rc={proc.returncode}). STDERR: {proc.stderr}"
        except Exception as e:
            return f"Error: Failed to call LLM backend: {e}"
    else:
        return f"Error: Unknown backend {backend!r}."


def _clusters(cxdb_path: pathlib.Path) -> list[Cluster]:
    db = CXDB(cxdb_path)
    rows = db.failed_steps()
    out: list[Cluster] = []
    for row in rows:
        agg = db.cluster_aggregates(
            row["node"], row["outcome"], row["output_hash"]
        )
        out.append(
            Cluster(
                node=row["node"],
                outcome=row["outcome"],
                output_hash=row["output_hash"],
                hits=row["hits"],
                sample=row["sample"] or "",
                run_ids=(row["run_ids"] or "").split(","),
                total_tokens=agg["total_tokens"],
                total_cost_usd=agg["total_cost_usd"],
                total_wall_ms=agg["total_wall_ms"],
            )
        )
    db.close()
    return out


def _fmt(value) -> str:
    if value is None:
        return "—"
    if isinstance(value, float):
        return f"{value:.4f}"
    return str(value)


def report(cxdb_path: pathlib.Path, backend: str = "echo") -> str:
    clusters = _clusters(cxdb_path)
    if not clusters:
        return "# Healer Report\n\nNo terminal failures recorded. Nothing to diagnose.\n"

    lines = [
        "# Healer Report",
        "",
        f"Source CXDB: `{cxdb_path}`",
        f"Failure clusters: **{len(clusters)}**",
        "",
        "| Hits | Node | Outcome | Hash | Total tokens | Total cost (USD) | Total wall (ms) | Prescription |",
        "|-----:|------|---------|------|-------------:|-----------------:|----------------:|--------------|",
    ]
    for c in clusters:
        lines.append(
            "| {hits} | `{node}` | `{outcome}` | `{hash}` | {tok} | {cost} | {wall} | {rx} |".format(
                hits=c.hits,
                node=c.node,
                outcome=c.outcome,
                hash=c.output_hash,
                tok=_fmt(c.total_tokens),
                cost=_fmt(c.total_cost_usd),
                wall=_fmt(c.total_wall_ms),
                rx=c.prescription(backend=backend, cxdb_path=cxdb_path),
            )
        )
    lines.append("")
    lines.append("## Sample output (per cluster, first 280 chars)")
    lines.append("")
    for c in clusters:
        lines.append(f"### `{c.node}` × `{c.outcome}` (hash `{c.output_hash}`)")
        lines.append("```")
        lines.append((c.sample or "")[:280] or "<empty>")
        lines.append("```")
        lines.append("")
    return "\n".join(lines)


def main(argv: Optional[list[str]] = None) -> int:
    _agy_safe.install()
    p = argparse.ArgumentParser(prog="dark-factory-healer")
    p.add_argument(
        "--cxdb",
        type=pathlib.Path,
        default=pathlib.Path.home() / ".dark-factory" / "cxdb.sqlite",
    )
    p.add_argument(
        "--backend",
        choices=["echo", "claude"],
        default="echo",
        help="LLM backend for healer diagnosis (default: echo)",
    )
    p.add_argument(
        "--out",
        type=pathlib.Path,
        default=None,
        help="Output file (default: stdout)",
    )
    args = p.parse_args(argv)

    if not args.cxdb.exists():
        print(f"CXDB not found: {args.cxdb}", file=sys.stderr)
        return 1

    text = report(args.cxdb, backend=args.backend)
    if args.out:
        args.out.write_text(text)
        print(f"wrote {args.out}")
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
