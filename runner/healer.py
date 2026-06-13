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
    # Snapshot of the first matching step's metadata_json (decoded). Surfaced
    # so the infra-vs-real discriminator (orch-g06) can read backend_missing
    # / timeout signatures without re-querying CXDB. Optional: empty dict when
    # the cluster was hand-built for unit tests.
    metadata: dict | None = None

    # Heuristic over the Healer's own node-name namespace (not user/model
    # input), so prefix matching here is internal data routing, not ZFC.
    @property
    def is_gate(self) -> bool:
        return self.node.startswith("gate_") or self.node in {"holdout", "validate"}

    def prescription(self, backend: str = "echo", cxdb_path: Optional[pathlib.Path] = None) -> str:
        return diagnose_cluster(self, backend, cxdb_path)

    def kind(self) -> tuple[str, str]:
        """Infra-vs-real classification of this cluster's failure mode.

        Returns (kind, justification) where kind ∈ {"infra", "real"}.
        Operators read this to decide whether the next action is to fix the
        harness/runner (infra) or the spec/prompt/holdout (real).

        Pure function over (outcome, sample, metadata); does not call out to
        CXDB or any model API.
        """
        return classify_failure_kind(self)


# Substrings in captured stdout/stderr that mean the agent never got to do
# work — the harness itself failed before any reasoning. These are checked
# case-insensitively against `cluster.sample`.
_INFRA_OUTPUT_MARKERS = (
    "command not found",       # shell: missing CLI on PATH
    "FileNotFoundError",       # missing file at runtime
    "ModuleNotFoundError",     # missing Python dep
    "backend_missing=true",    # explicit sentinel in metadata-as-text
    "No such file or directory",
)


def classify_failure_kind(cluster: Cluster) -> tuple[str, str]:
    """Classify a failure cluster as 'infra' or 'real'.

    Heuristic ordering:
      1. If metadata has backend_missing=true / FileNotFoundError /
         "command not found" / ModuleNotFoundError markers in sample →
         "infra" with the matching marker as the reason.
      2. outcome == "exhausted" → "real" (agent looped, real work happened).
      3. outcome in {failure, fail, partial, inconclusive} with non-empty
         output → "real".
      4. outcome in {stuck, error} with empty output_head (or short
         timeout-style metadata) → "infra".
      5. Fallback: "real" with a generic reason.
    """
    metadata = cluster.metadata or {}
    sample = cluster.sample or ""
    sample_lc = sample.lower()
    metadata_str = " ".join(
        f"{k}={v}" for k, v in metadata.items()
    ).lower()

    # 1. Explicit infra markers — sample takes priority (it's what the
    # operator actually saw) but we also check metadata-in-as-text because
    # some code paths drop the marker into metadata_json as a string.
    for marker in _INFRA_OUTPUT_MARKERS:
        if marker.lower() in sample_lc or marker.lower() in metadata_str:
            return ("infra", f"infra marker in output/metadata: {marker}")

    # 2. exhausted = the agent loop hit max_visits. Real work happened.
    if cluster.outcome == "exhausted":
        return ("real", "agent looped until max_visits — real work happened")

    # 3. Normal real-failure outcomes with substantive output.
    if cluster.outcome in {"failure", "fail", "partial", "inconclusive"}:
        if sample.strip():
            return ("real", f"outcome={cluster.outcome} with normal output")
        # empty output on a real-shaped outcome is unusual — fall through.

    # 4. stuck / error with empty sample (or timeout-style metadata) → infra.
    if cluster.outcome in {"stuck", "error"}:
        if not sample.strip():
            return ("infra", f"outcome={cluster.outcome} with empty output")
        if "timeout" in metadata_str or metadata.get("timeout_ms"):
            return ("infra", f"outcome={cluster.outcome} with timeout signature")
        # stuck with substantive output is a real (non-converging) failure.
        return ("real", f"outcome={cluster.outcome} with substantive output")

    # 5. Fallback — treat as real; the discriminator is conservative.
    return ("real", f"outcome={cluster.outcome} (no infra signature detected)")


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
        # Snapshot first matching step's metadata_json for the discriminator
        # (orch-g06). The Cluster dataclass accepts a dict-shaped metadata.
        first_meta: dict = {}
        try:
            cur = db._conn.cursor()
            cur.execute(
                "SELECT metadata_json FROM steps WHERE node=? AND outcome=? AND output_hash=? LIMIT 1",
                (row["node"], row["outcome"], row["output_hash"]),
            )
            meta_row = cur.fetchone()
            if meta_row and meta_row[0]:
                import json as _json
                try:
                    first_meta = _json.loads(meta_row[0])
                except (TypeError, ValueError):
                    first_meta = {}
        except Exception:
            first_meta = {}
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
                metadata=first_meta,
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

    # Pre-compute infra/real labels (orch-g06) so the summary and the table
    # share the same classification. Skipping pre-compute and recomputing in
    # the row loop would be cheaper but harder to audit.
    classified = [(c, *c.kind()) for c in clusters]
    n_infra = sum(1 for _, k, _ in classified if k == "infra")
    n_real = sum(1 for _, k, _ in classified if k == "real")

    lines = [
        "# Healer Report",
        "",
        f"Source CXDB: `{cxdb_path}`",
        f"Failure clusters: **{len(clusters)}** "
        f"(infra: **{n_infra}**, real: **{n_real}**)",
        "",
        "| Hits | Node | Outcome | Kind | Justification | Hash | Total tokens | Total cost (USD) | Total wall (ms) | Prescription |",
        "|-----:|------|---------|------|---------------|------|-------------:|-----------------:|----------------:|--------------|",
    ]
    for c, kind, reason in classified:
        lines.append(
            "| {hits} | `{node}` | `{outcome}` | **{kind}** | {reason} | `{hash}` | {tok} | {cost} | {wall} | {rx} |".format(
                hits=c.hits,
                node=c.node,
                outcome=c.outcome,
                kind=kind,
                reason=_md_escape(reason),
                hash=c.output_hash,
                tok=_fmt(c.total_tokens),
                cost=_fmt(c.total_cost_usd),
                wall=_fmt(c.total_wall_ms),
                rx=c.prescription(backend=backend, cxdb_path=cxdb_path),
            )
        )
    lines.append("")
    lines.append("## Failure kind — operational guidance")
    lines.append("")
    lines.append(
        "- **infra** — the harness/runner itself failed (backend missing, "
        "CLI not on PATH, file/dependency absent, hang/timeout). Fix the "
        "harness or the runtime environment before retrying.")
    lines.append(
        "- **real** — the agent ran and produced a wrong / partial / "
        "exhausted result. Fix the spec, prompt template, or holdout "
        "scenario.")
    lines.append("")
    lines.append("## Sample output (per cluster, first 280 chars)")
    lines.append("")
    for c, kind, _reason in classified:
        lines.append(
            f"### `{c.node}` × `{c.outcome}` (hash `{c.output_hash}`) — **{kind}**"
        )
        lines.append("```")
        lines.append((c.sample or "")[:280] or "<empty>")
        lines.append("```")
        lines.append("")
    return "\n".join(lines)


def _md_escape(text: str) -> str:
    """Escape pipe + newline so justification text doesn't break the table row."""
    if not text:
        return ""
    return text.replace("|", "\\|").replace("\n", " ")


def main(argv: Optional[list[str]] = None) -> int:
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
