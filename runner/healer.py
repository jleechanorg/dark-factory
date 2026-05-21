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

    @property
    def is_gate(self) -> bool:
        return self.node.startswith("gate_") or self.node in {"holdout", "validate"}

    def prescription(self) -> str:
        if self.node in {"plan", "implement", "fix"}:
            return (
                f"prompt template at `prompts/<feature>/{self.node}.md` returned "
                f"`{self.outcome}` — review prompt or upstream spec."
            )
        if self.is_gate:
            return (
                f"gate `{self.node}` returned `{self.outcome}` — re-inspect "
                "evidence bundle / diff against the relevant slash-command standards."
            )
        if self.node == "holdout":
            return (
                "holdout evaluator reported failure — sealed scenarios diverge from "
                "implementation; do NOT inspect scenarios. Re-read spec + impl."
            )
        return (
            f"node `{self.node}` failed with `{self.outcome}` ({self.hits} run(s)). "
            "Check handler configuration and node attributes."
        )


def _clusters(cxdb_path: pathlib.Path) -> list[Cluster]:
    db = CXDB(cxdb_path)
    rows = db.failed_steps()
    out: list[Cluster] = []
    for row in rows:
        out.append(
            Cluster(
                node=row["node"],
                outcome=row["outcome"],
                output_hash=row["output_hash"],
                hits=row["hits"],
                sample=(row["sample"] or "")[:280],
                run_ids=(row["run_ids"] or "").split(","),
            )
        )
    db.close()
    return out


def report(cxdb_path: pathlib.Path) -> str:
    clusters = _clusters(cxdb_path)
    if not clusters:
        return "# Healer Report\n\nNo terminal failures recorded. Nothing to diagnose.\n"

    lines = [
        "# Healer Report",
        "",
        f"Source CXDB: `{cxdb_path}`",
        f"Failure clusters: **{len(clusters)}**",
        "",
        "| Hits | Node | Outcome | Hash | Prescription |",
        "|-----:|------|---------|------|--------------|",
    ]
    for c in clusters:
        lines.append(
            f"| {c.hits} | `{c.node}` | `{c.outcome}` | `{c.output_hash}` | {c.prescription()} |"
        )
    lines.append("")
    lines.append("## Sample output (per cluster, first 280 chars)")
    lines.append("")
    for c in clusters:
        lines.append(f"### `{c.node}` × `{c.outcome}` (hash `{c.output_hash}`)")
        lines.append("```")
        lines.append(c.sample or "<empty>")
        lines.append("```")
        lines.append("")
    return "\n".join(lines)


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="dark-factory-healer")
    p.add_argument(
        "--cxdb",
        type=pathlib.Path,
        default=pathlib.Path.home() / ".dark-factory" / "cxdb.sqlite",
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

    text = report(args.cxdb)
    if args.out:
        args.out.write_text(text)
        print(f"wrote {args.out}")
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
