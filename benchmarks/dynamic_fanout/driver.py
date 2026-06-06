"""Run the deterministic separation trials and grade them with the SAME
aggregator used by workflow_graphgen.

Each (feature, mode, trial) runs in an isolated workdir, produces real files,
and is graded by re-reading those files. Records are emitted in the exact shape
``benchmarks.workflow_graphgen.scoring.assemble_record`` produces, so
``aggregate`` applies the identical range-non-overlap + MIN_N_FOR_WINNER rule
that returned the n=10 null — that reuse is the whole calibration argument.
"""

from __future__ import annotations

import json
import pathlib
import tempfile
import time

from benchmarks.workflow_graphgen.scoring import assemble_record, aggregate
from . import modes

#: Feature catalog. Each entry: (scenario, gap_tier, per-trial kwargs builder).
#: The builder takes the trial index and returns kwargs for run_mode_a/apb.
FEATURES = {
    # G3 paradigm gap: K endpoints > Mode A's fixed node count -> A under-covers.
    "validate_k6": ("validate", "G3", lambda t: {"k": 6, "fixed": modes.FIXED_NODES}),
    # G3 efficiency face: K endpoints < fixed node count -> A wastes dispatches.
    "validate_k2": ("validate", "G3", lambda t: {"k": 2, "fixed": modes.FIXED_NODES}),
    # G1 engine gap: migration must match schema columns; A has no state to thread.
    # Per-trial columns vary (and never equal Mode A's static default) so A always
    # drifts while A+B always matches.
    "schema_migration": (
        "schema",
        "G1",
        lambda t: {"schema_columns": ["id", f"field_{t}", "updated_at"]},
    ),
    # G1 with Mode A given its BEST honest config (${state._last_output} threading):
    # A now matches the schema and TIES A+B -> the G1 win evaporates, proving it is
    # an authoring gap, not a paradigm gap. Contrast with `schema_migration` above.
    "schema_migration_threaded": (
        "schema_threaded",
        "G1",
        lambda t: {"schema_columns": ["id", f"field_{t}", "updated_at"]},
    ),
}


def _one(feature: str, mode: str, trial: int, workroot: pathlib.Path) -> dict:
    scenario, _tier, kw_builder = FEATURES[feature]
    kw = kw_builder(trial)
    workdir = pathlib.Path(tempfile.mkdtemp(prefix=f"{feature}_{mode}_{trial}_", dir=workroot))
    runner = modes.run_mode_a if mode == "A" else modes.run_mode_apb
    t0 = time.perf_counter()
    res = runner(scenario, workdir, **kw)
    wall_ms = int((time.perf_counter() - t0) * 1000)
    return assemble_record(
        feature=feature,
        mode=mode,
        trial=trial,
        backend="deterministic",
        model_name=None,
        conformance=res["conformance"],
        tokens={"tokens_in": res["tokens_in"], "tokens_out": res["tokens_out"], "wall_ms": wall_ms},
        wall_ms_total=wall_ms,
        graph_quality_score={"score": None, "unscored": True},
        zero_touch=True,
        loop_bounds_held=True,
        baseline_ref="deterministic",
        head_ref="deterministic",
        diff_empty=False,
        exploratory=False,
        gen_tokens=None,
        notes=f"scenario={scenario} calls={res['calls']}",
    )


def run(features, modes_list, trials: int, workroot: str | pathlib.Path) -> list[dict]:
    workroot = pathlib.Path(workroot)
    workroot.mkdir(parents=True, exist_ok=True)
    records = []
    for feature in features:
        for mode in modes_list:
            for trial in range(trials):
                records.append(_one(feature, mode, trial, workroot))
    return records


def write_records(records: list[dict], out_path: str | pathlib.Path) -> dict:
    out_path = pathlib.Path(out_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w") as fh:
        for r in records:
            fh.write(json.dumps(r) + "\n")
    agg = aggregate(records)
    agg_path = out_path.with_suffix(".aggregate.json")
    agg_path.write_text(json.dumps(agg, indent=2) + "\n")
    return agg
