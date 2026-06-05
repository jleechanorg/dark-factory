"""CLI: run the deterministic dynamic_fanout separation benchmark.

    python -m benchmarks.dynamic_fanout --trials 5 \
        --out /tmp/dynfan/records.jsonl

Prints a per-feature, per-axis rollup. Because the scenarios are deterministic,
n=5 is already conclusive: every trial of a cell is byte-identical, so the
ranges are single points and the SAME aggregator that returned the
workflow_graphgen n=10 null now credits the winners below.
"""

from __future__ import annotations

import argparse
import pathlib

from .driver import FEATURES, run, write_records

_TIER = {f: FEATURES[f][1] for f in FEATURES}


def _fmt_axis(row) -> str:
    win = row.get("winner")
    if win:
        verdict = f"WINNER={win}"
    elif row.get("apparent_winner"):
        verdict = f"apparent={row['apparent_winner']} (not credited)"
    else:
        verdict = row["result"]
    sa, sb = row.get("stats_A"), row.get("stats_AB")

    def m(s):
        return f"{s['mean']:.3g}" if s else "—"

    return f"  {row['axis']:14s} A={m(sa):>8s} A+B={m(sb):>8s}  {verdict}"


def main(argv=None):
    ap = argparse.ArgumentParser(prog="benchmarks.dynamic_fanout")
    ap.add_argument("--features", default=",".join(FEATURES))
    ap.add_argument("--modes", default="A,A+B")
    ap.add_argument("--trials", type=int, default=5)
    ap.add_argument("--workroot", default="/tmp/dynfan/work")
    ap.add_argument("--out", default="/tmp/dynfan/records.jsonl")
    args = ap.parse_args(argv)

    features = [f for f in args.features.split(",") if f]
    modes_list = [m for m in args.modes.split(",") if m]
    records = run(features, modes_list, args.trials, args.workroot)
    agg = write_records(records, args.out)

    print(f"\ndynamic_fanout — {args.trials} trials/cell · {len(records)} records")
    print(f"records: {args.out}")
    print(f"aggregate: {pathlib.Path(args.out).with_suffix('.aggregate.json')}\n")
    credited = 0
    for feature in features:
        print(f"{feature}  [{_TIER.get(feature, '?')} gap]")
        for row in agg["results"]:
            if row["feature"] != feature:
                continue
            # only show axes that carry signal or a credited winner
            if row["axis"] in ("conformance", "tokens_total", "wall_ms"):
                print(_fmt_axis(row))
                if row.get("winner"):
                    credited += 1
        print()
    print(f"credited winners (range-disjoint, n>=MIN_N): {credited}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
