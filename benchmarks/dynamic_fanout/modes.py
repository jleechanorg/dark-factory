"""The two dispatch strategies under test. Same coder, different orchestration.

Mode A models a **static** authored graph: a fixed node count, prompts that read
only ``${goal}`` (no inter-node state). Mode A+B models a harness that can
**discover** runtime quantities and **thread** one node's output into the next.

Nothing here calls an LLM. The token/cost figures are a deterministic call-count
model (each coder dispatch costs a fixed token budget), which is exactly the
quantity that differs between "run F fixed nodes" and "fan out to K discovered
nodes". Wall time is measured by the driver, not modeled here.
"""

from __future__ import annotations

import pathlib

from .scenario import make_api_source, count_endpoints
from .coder_mock import add_validation, write_schema, write_migration
from .evaluator import coverage, consistency

#: Mode A's authored, static implement-node count for the validate scenario.
#: A static .dot cannot change this at runtime — that is the whole point.
FIXED_NODES = 3

#: Deterministic per-dispatch token budget (identical coder both modes).
TOKENS_IN_PER_CALL = 1200
TOKENS_OUT_PER_CALL = 150

#: Mode A's best static guess for the migration columns. Because A's migration
#: node sees only ``${goal}`` it must hardcode a guess; any per-trial schema that
#: differs from this drifts. (Chosen to differ from the driver's trial schemas.)
DEFAULT_SCHEMA_COLUMNS = ["id", "name", "created_at"]


# ---- G3: runtime-determined fan-out (validate_endpoints) -----------------

def _validate_result(workdir, calls: int) -> dict:
    return {
        "calls": calls,
        "tokens_in": calls * TOKENS_IN_PER_CALL,
        "tokens_out": calls * TOKENS_OUT_PER_CALL,
        "conformance": coverage(workdir),
    }


def run_mode_a_validate(workdir: str | pathlib.Path, k: int, fixed: int = FIXED_NODES) -> dict:
    """Static graph: dispatch exactly ``fixed`` implement nodes (endpoints
    0..fixed-1). Covers ``min(fixed, k)`` endpoints; spends ``fixed`` calls
    regardless of k (nodes past k are wasted dispatches)."""
    make_api_source(workdir, k)
    for i in range(fixed):
        add_validation(workdir, i)  # no-op (but still a dispatch) when i >= k
    return _validate_result(workdir, calls=fixed)


def run_mode_apb_validate(workdir: str | pathlib.Path, k: int) -> dict:
    """Discover k from the source, fan out to exactly k implement nodes. Covers
    all k endpoints; spends k calls."""
    make_api_source(workdir, k)
    discovered = count_endpoints(workdir)  # the runtime discovery A cannot do
    for i in range(discovered):
        add_validation(workdir, i)
    return _validate_result(workdir, calls=discovered)


# ---- G1: inter-node state threading (schema_migration) -------------------

def _schema_result(workdir, calls: int) -> dict:
    return {
        "calls": calls,
        "tokens_in": calls * TOKENS_IN_PER_CALL,
        "tokens_out": calls * TOKENS_OUT_PER_CALL,
        "conformance": consistency(workdir),
    }


def run_mode_a_schema(workdir: str | pathlib.Path, schema_columns: list[str]) -> dict:
    """NAIVE Mode A — node 2's prompt interpolates only ``${goal}``.

    Node 1 writes the real schema; node 2 (migration) sees only the goal, so it
    falls back to ``DEFAULT_SCHEMA_COLUMNS`` and drifts unless the trial's schema
    happens to equal the default. This is Mode A's *worst* honest config and is
    kept only as a contrast against ``run_mode_a_schema_threaded``.
    """
    make_dir(workdir)
    write_schema(workdir, schema_columns)            # node 1 output
    write_migration(workdir, DEFAULT_SCHEMA_COLUMNS)  # node 2: blind to node 1
    return _schema_result(workdir, calls=2)


def run_mode_a_schema_threaded(workdir: str | pathlib.Path, schema_columns: list[str]) -> dict:
    """HONEST Mode A — node 2's prompt interpolates ``${state._last_output}``.

    This models a static ``.dot`` whose migration node references the previous
    node's output via the runner's ``${state._last_output}`` placeholder (engine
    writes it after every node — proven by
    ``tests/test_state_threading.py::test_last_output_threads_into_next_node``).
    Adjacent inter-node data flow therefore needs **no engine change**; it is a
    prompt-authoring choice. Given this best honest config, Mode A matches the
    schema and **ties Mode A+B** — demonstrating G1 is an authoring gap, not a
    paradigm gap. (Only G3, runtime-determined node count, survives an honest A.)
    """
    make_dir(workdir)
    threaded = write_schema(workdir, schema_columns)  # node 1 output -> _last_output
    write_migration(workdir, threaded)                # node 2 reads ${state._last_output}
    return _schema_result(workdir, calls=2)


def run_mode_apb_schema(workdir: str | pathlib.Path, schema_columns: list[str]) -> dict:
    """State threaded: node 1's column list is fed into node 2's input, so the
    migration matches the schema."""
    make_dir(workdir)
    threaded = write_schema(workdir, schema_columns)  # node 1 output -> state
    write_migration(workdir, threaded)                # node 2 reads state
    return _schema_result(workdir, calls=2)


def make_dir(workdir):
    pathlib.Path(workdir).mkdir(parents=True, exist_ok=True)


# ---- unified entry points (dispatch by scenario) -------------------------

def run_mode_a(scenario: str, workdir, **kw) -> dict:
    if scenario == "validate":
        return run_mode_a_validate(workdir, k=kw["k"], fixed=kw.get("fixed", FIXED_NODES))
    if scenario == "schema":  # naive A: ${goal}-only prompt
        return run_mode_a_schema(workdir, schema_columns=kw["schema_columns"])
    if scenario == "schema_threaded":  # honest A: ${state._last_output} prompt
        return run_mode_a_schema_threaded(workdir, schema_columns=kw["schema_columns"])
    raise ValueError(f"unknown scenario {scenario!r}")


def run_mode_apb(scenario: str, workdir, **kw) -> dict:
    if scenario == "validate":
        return run_mode_apb_validate(workdir, k=kw["k"])
    if scenario in ("schema", "schema_threaded"):
        return run_mode_apb_schema(workdir, schema_columns=kw["schema_columns"])
    raise ValueError(f"unknown scenario {scenario!r}")
