"""Scoring for workflow_graphgen: graph_quality, token accounting, record shape.

Axes (spec §"Scored axes"): conformance, cost (tokens_in/out + wall_ms),
graph_quality, zero_touch.

Key fairness rules encoded here:
  * graph_quality grades the **shared** graph-IR (generated once per goal), so it
    is mode-invariant by construction. The deterministic 70% (presence 35% +
    edge_validity 35%) is computed in code. The 30% node_selection_fit part is a
    live reviewer judgment scored ONCE per goal and reused for both modes; when no
    fit score is available it is recorded unscored (deterministic-70 partial),
    never a silent zero.
  * token accounting reads the IDENTICAL coder-execution fields for both modes
    (TOKEN_FIELDS), since both run the same claude/Sonnet `_codergen`.
"""

from __future__ import annotations

import pathlib
import statistics
import tempfile

from .graph_ir import GUARANTEED_NODES, GraphIR, render_full_dot

# The identical coder-execution token fields both modes read from `_codergen`
# Result metadata. A unit test asserts both modes pull exactly these.
TOKEN_FIELDS = ("tokens_in", "tokens_out", "wall_ms")

PRESENCE_WEIGHT = 0.35
EDGE_VALIDITY_WEIGHT = 0.35
FIT_WEIGHT = 0.30


def _coerce_num(value):
    """Coerce a stringified metadata value to int/float, or None when absent."""
    if value is None:
        return None
    if isinstance(value, (int, float)):
        return value
    s = str(value).strip()
    if s == "" or s.lower() == "none":
        return None
    try:
        return int(s)
    except ValueError:
        try:
            return float(s)
        except ValueError:
            return None


def read_token_record(metadata: dict) -> dict:
    """Extract the canonical token fields from a `_codergen` Result.metadata.

    Returns a dict keyed by exactly TOKEN_FIELDS (values may be None when the
    backend emitted no usage banner). Identical for both modes by construction.
    """
    return {field: _coerce_num(metadata.get(field)) for field in TOKEN_FIELDS}


def sum_token_records(records: list[dict]) -> dict:
    """Sum a list of per-node token records over the dynamic middle.

    None values are treated as missing: a field's sum is None only if every
    record's value was None, otherwise missing entries contribute 0 so a partial
    banner still aggregates (mirrors `_codergen_metrics` None-vs-zero semantics).
    """
    out = {}
    for field in TOKEN_FIELDS:
        vals = [r.get(field) for r in records]
        present = [v for v in vals if v is not None]
        out[field] = sum(present) if present else None
    return out


def graph_quality(ir: GraphIR, fit_score: int | None = None) -> dict:
    """Score the shared graph-IR. Deterministic 70% computed in code; 30% fit is
    the supplied once-per-goal reviewer judgment (or unscored).

    Returns a dict with the component parts and a final ``score`` (0..100) or a
    ``partial`` deterministic-70 with ``unscored=True`` when fit is unavailable.
    """
    # presence (35%): both reviewer nodes present AND reachable from start.
    # edge_validity (35%): the IR renders to a parser-valid graph.
    presence = False
    edge_validity = False
    try:
        with tempfile.TemporaryDirectory() as td:
            dot_path = pathlib.Path(td) / "q.dot"
            dot_path.write_text(render_full_dot(ir))
            import runner.parser as parser_mod

            graph = parser_mod.parse(dot_path)  # raises unless reachable + valid
            edge_validity = True
            reachable = _reachable_from_start(graph)
            presence = all(name in graph.nodes and name in reachable for name in GUARANTEED_NODES)
    except Exception:
        presence = False
        edge_validity = False

    deterministic = (PRESENCE_WEIGHT if presence else 0.0) + (EDGE_VALIDITY_WEIGHT if edge_validity else 0.0)
    det_pct = round(deterministic * 100, 2)  # out of 70 max

    result = {
        "presence": presence,
        "edge_validity": edge_validity,
        "deterministic_70": det_pct,
        "fit": fit_score,
    }
    if fit_score is None:
        result["unscored"] = True
        result["score"] = None  # deterministic-70 partial, not a silent zero
        result["partial"] = det_pct
    else:
        fit = max(0, min(100, int(fit_score)))
        result["unscored"] = False
        result["score"] = round(det_pct + FIT_WEIGHT * fit, 2)
    return result


def _reachable_from_start(graph) -> set:
    start = "start" if "start" in graph.nodes else None
    if start is None:
        for name, node in graph.nodes.items():
            if node.shape == "Mdiamond":
                start = name
                break
    if start is None:
        return set()
    seen, stack = set(), [start]
    while stack:
        cur = stack.pop()
        if cur in seen:
            continue
        seen.add(cur)
        for e in graph.outgoing(cur):
            stack.append(e.dst)
    return seen


def assemble_record(
    *,
    feature: str,
    mode: str,
    trial: int,
    backend: str,
    model_name: str | None,
    conformance: dict,
    tokens: dict,
    wall_ms_total: int | None,
    graph_quality_score: dict,
    zero_touch: bool,
    loop_bounds_held: bool,
    baseline_ref: str,
    head_ref: str,
    diff_empty: bool,
    exploratory: bool,
    gen_tokens: dict | None = None,
    notes: str = "",
) -> dict:
    """Assemble one per-run benchmark record across the four axes.

    Field names match the specs (notably ``zero_touch``, the canonical robustness
    field name shared with the main spec).
    """
    return {
        "feature": feature,
        "mode": mode,  # "A" | "A+B"
        "trial": trial,
        "backend": backend,
        "model_name": model_name,
        "exploratory": exploratory,
        # axis 1: conformance (from sealed evaluator aggregate line)
        "conformance": conformance,  # {pass, total, available}
        # axis 2: cost (coder-execution tokens for the middle, equal footing)
        "tokens": tokens,  # {tokens_in, tokens_out, wall_ms}
        "wall_ms": wall_ms_total,
        # generator cost recorded separately and EXCLUDED from middle cost (§9b);
        # None for the deterministic generator (no Opus call), populated when a
        # live Opus Workflow generator is plugged in.
        "gen_tokens": gen_tokens,
        # axis 3: graph quality (shared IR; mode-invariant)
        "graph_quality": graph_quality_score,
        # axis 4: robustness / zero-touch
        "zero_touch": zero_touch,
        "loop_bounds_held": loop_bounds_held,
        # diff handoff provenance
        "baseline_ref": baseline_ref,
        "head_ref": head_ref,
        "diff_empty": diff_empty,
        "notes": notes,
    }


# ---- aggregation (§11.4 directional signal) ----------------------------

def _axis_value(record: dict, axis: str):
    """Extract a single comparable scalar for ``axis`` from a record, or None."""
    if axis == "conformance":
        c = record.get("conformance") or {}
        if not c.get("available"):
            return None
        total = _coerce_num(c.get("total"))
        passed = _coerce_num(c.get("pass"))
        if total in (None, 0) or passed is None:
            return None
        return passed / total  # pass-rate, higher is better
    if axis == "tokens_total":
        t = record.get("tokens") or {}
        ti, to = _coerce_num(t.get("tokens_in")), _coerce_num(t.get("tokens_out"))
        if ti is None and to is None:
            return None
        return (ti or 0) + (to or 0)  # lower is better
    if axis == "graph_quality":
        return _coerce_num((record.get("graph_quality") or {}).get("score"))
    if axis == "zero_touch":
        v = record.get("zero_touch")
        return 1.0 if v is True else (0.0 if v is False else None)
    if axis == "wall_ms":
        return _coerce_num(record.get("wall_ms"))  # lower is better
    return None


# Direction: True = higher is better, False = lower is better.
_AXIS_HIGHER_BETTER = {
    "conformance": True,
    "tokens_total": False,
    "graph_quality": True,
    "zero_touch": True,
    "wall_ms": False,
}

# A per-axis winner is only declared once each mode has at least this many
# trials. Below it, even non-overlapping ranges are statistically underpowered
# (at n=1 two distinct points ALWAYS "fail to overlap" — pure noise can manufacture
# a confident-looking winner), so we refuse to name one.
MIN_N_FOR_WINNER = 5


def _stats(values: list) -> dict | None:
    """Summary stats over the present (non-None) values of one mode/axis."""
    present = [v for v in values if v is not None]
    if not present:
        return None
    return {
        "n": len(present),
        "mean": round(statistics.fmean(present), 4),
        "stdev": round(statistics.stdev(present), 4) if len(present) > 1 else 0.0,
        "min": min(present),
        "max": max(present),
    }


def _range(values: list):
    present = [v for v in values if v is not None]
    if not present:
        return None
    return (min(present), max(present))


def aggregate_axis(records: list[dict], feature: str, axis: str) -> dict:
    """Aggregate one axis for one feature across modes A and A+B (§11.4).

    A per-axis winner is declared ONLY when the two modes' trial ranges do not
    overlap; overlapping ranges report ``no separation at n=<k>``. Missing data on
    either side reports ``insufficient data``.
    """
    by_mode = {"A": [], "A+B": []}
    for r in records:
        if r.get("feature") != feature:
            continue
        if r.get("exploratory"):
            continue
        if r.get("mode") in by_mode:
            by_mode[r["mode"]].append(_axis_value(r, axis))
    ra, rb = _range(by_mode["A"]), _range(by_mode["A+B"])
    n_a = len([v for v in by_mode["A"] if v is not None])
    n_b = len([v for v in by_mode["A+B"] if v is not None])
    n = min(n_a, n_b)
    out = {
        "feature": feature, "axis": axis,
        "range_A": ra, "range_AB": rb, "n": n,
        "stats_A": _stats(by_mode["A"]), "stats_AB": _stats(by_mode["A+B"]),
    }
    if ra is None or rb is None:
        out["winner"] = None
        out["result"] = "insufficient data"
        return out
    higher_better = _AXIS_HIGHER_BETTER[axis]
    # Non-overlapping ranges?  A strictly better than A+B, or vice versa.
    a_above_b = ra[0] > rb[1]
    b_above_a = rb[0] > ra[1]
    if not (a_above_b or b_above_a):
        out["winner"] = None
        out["result"] = f"no separation at n={n}"
        return out
    # Ranges separate — but a winner is only credible with enough trials. Below
    # MIN_N_FOR_WINNER, report the apparent direction WITHOUT crowning a winner.
    apparent = "A" if ((a_above_b and higher_better) or (b_above_a and not higher_better)) else "A+B"
    if n < MIN_N_FOR_WINNER:
        out["winner"] = None
        out["apparent_winner"] = apparent
        out["result"] = f"separated but underpowered (n={n} < {MIN_N_FOR_WINNER})"
        return out
    out["winner"] = apparent
    out["result"] = "separated"
    return out


def aggregate(records: list[dict]) -> dict:
    """Per-feature, per-axis directional-signal aggregation over all records."""
    features = sorted({r.get("feature") for r in records if r.get("feature")})
    axes = ("conformance", "tokens_total", "graph_quality", "zero_touch", "wall_ms")
    return {
        "features": features,
        "axes": list(axes),
        "results": [aggregate_axis(records, f, a) for f in features for a in axes],
    }
