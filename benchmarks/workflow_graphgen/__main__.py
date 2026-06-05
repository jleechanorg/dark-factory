"""CLI entrypoint for the workflow_graphgen A-vs-A+B benchmark.

Usage (run from the dark-factory checkout):

    python -m benchmarks.workflow_graphgen \
        --features hello,roman --modes A,A+B --trials 1 \
        --backend claude --model claude-sonnet-4-6 \
        --workroot /tmp/wfgg-smoke --out /tmp/wfgg-smoke/records.jsonl

The single independent variable across A and A+B is *who runs the dynamic
middle*. Every other control (shared IR, baseline_ref handoff, identical
reviewer spine, identical coder backend/model) is held fixed by the harness.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

from .harness import SMOKE_GOALS, run_benchmark
from .scoring import aggregate


def _csv(value: str) -> list[str]:
    return [p.strip() for p in value.split(",") if p.strip()]


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="benchmarks.workflow_graphgen")
    ap.add_argument("--features", type=_csv, default=["hello", "roman"],
                    help="comma-separated feature goals (default: hello,roman)")
    ap.add_argument("--modes", type=_csv, default=["A", "A+B"],
                    help="comma-separated modes (default: A,A+B)")
    ap.add_argument("--trials", type=int, default=1)
    ap.add_argument("--backend", default="claude")
    ap.add_argument("--model", dest="model_name", default="claude-sonnet-4-6")
    ap.add_argument("--workroot", type=pathlib.Path, default=pathlib.Path("/tmp/wfgg-smoke"))
    ap.add_argument("--out", dest="out_path", type=pathlib.Path,
                    default=pathlib.Path("/tmp/wfgg-smoke/records.jsonl"))
    args = ap.parse_args(argv)

    unknown = [f for f in args.features if f not in SMOKE_GOALS]
    if unknown:
        ap.error(f"no smoke goal defined for: {', '.join(unknown)} "
                 f"(known: {', '.join(sorted(SMOKE_GOALS))})")

    records = run_benchmark(
        features=args.features,
        modes=args.modes,
        trials=args.trials,
        backend=args.backend,
        model_name=args.model_name,
        workroot=args.workroot,
        out_path=args.out_path,
    )

    # Compact human-readable rollup to stderr; machine records went to --out.
    print(f"wrote {len(records)} records -> {args.out_path}", file=sys.stderr)
    for r in records:
        tok = r["tokens"]
        print(
            f"  {r['feature']:6s} mode={r['mode']:3s} trial={r['trial']} "
            f"zero_touch={r['zero_touch']!s:5s} diff_empty={r['diff_empty']!s:5s} "
            f"gq70={r['graph_quality']['deterministic_70']} "
            f"tok_in={tok['tokens_in']} tok_out={tok['tokens_out']} wall_ms={tok['wall_ms']}",
            file=sys.stderr,
        )
    # §11.4 directional-signal aggregation (winner only on non-overlapping ranges).
    agg = aggregate(records)
    print("aggregation (directional signal):", file=sys.stderr)
    for row in agg["results"]:
        print(
            f"  {row['feature']:6s} {row['axis']:14s} -> {row['result']}"
            + (f" (winner={row['winner']})" if row.get("winner") else ""),
            file=sys.stderr,
        )
    if args.out_path is not None:
        agg_path = args.out_path.with_name(args.out_path.stem + ".aggregate.json")
        agg_path.write_text(json.dumps(agg, indent=2))
        print(f"wrote aggregate -> {agg_path}", file=sys.stderr)

    # Also emit the full JSON array on stdout for piping.
    print(json.dumps({"records": records, "aggregate": agg}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
