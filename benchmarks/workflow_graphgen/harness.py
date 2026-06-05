"""Mode A / Mode A+B executors + benchmark driver for workflow_graphgen.

The independent variable is *who runs the dynamic middle*:
  * Mode A   — the Python runner walks a middle-only graph (start -> middle -> exit).
  * Mode A+B — the harness calls the same `_codergen` per middle node directly.

Everything else is held identical: both modes share one generated graph-IR, record
the same ``baseline_ref`` before the middle, commit the combined middle diff, then
run the SAME terminal-reviewer spine through the runner. Both read the identical
coder-execution token fields (the same `_codergen` claude/Sonnet path).
"""

from __future__ import annotations

import json
import pathlib
import shutil
import subprocess
import tempfile
import time

import runner.engine as engine_mod
import runner.parser as parser_mod
from runner.handlers import Context, _codergen
from runner.parser import Node

from .catalog import validate_node_prompts
from .generator import DEFAULT_CODER_BACKEND, DEFAULT_CODER_MODEL, deterministic_generator
from .graph_ir import (
    GraphIR,
    render_middle_only_dot,
    render_spine_dot,
)
from .scoring import (
    assemble_record,
    graph_quality,
    read_token_record,
    sum_token_records,
)


def _git(workdir: pathlib.Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(workdir), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"git {list(args)} failed in {workdir} (rc={proc.returncode}): {proc.stderr.strip()}"
        )
    return proc.stdout.strip()


def _head(workdir: pathlib.Path) -> str:
    return _git(workdir, "rev-parse", "HEAD")


def _assert_no_cycles(graph) -> None:
    """DFS cycle check — cyclic graphs allow Mode A to revisit nodes while Mode A+B
    iterates middle_chain() once, breaking A/A+B call-count symmetry."""
    visited: set[str] = set()
    rec_stack: set[str] = set()

    def dfs(name: str) -> None:
        visited.add(name)
        rec_stack.add(name)
        for edge in graph.edges:
            if edge.src == name:
                if edge.dst not in visited:
                    dfs(edge.dst)
                elif edge.dst in rec_stack:
                    raise AssertionError(
                        f"graph has cycle through {name!r} → {edge.dst!r} — "
                        "Mode A runner would revisit nodes, breaking A/A+B symmetry"
                    )
        rec_stack.discard(name)

    for node_name in graph.nodes:
        if node_name not in visited:
            dfs(node_name)


def assert_no_retry_wiring(graph) -> None:
    """Fairness assertion (spec §5): in BOTH modes neither any guaranteed reviewer
    node nor the graph itself may enable a goal_gate, declare a retry_target,
    use max_visits>1, or contain cycles — all of which let Mode A visit a node
    more than once while Mode A+B executes each middle node exactly once."""
    if str(graph.attrs.get("retry_target", "")):
        raise AssertionError("graph declares a retry_target — breaks A/A+B symmetry")
    for name, node in graph.nodes.items():
        gg = node.attrs.get("goal_gate")
        if isinstance(gg, bool) and gg:
            raise AssertionError(f"node {name!r} enables goal_gate — breaks A/A+B symmetry")
        if str(gg).lower() == "true":
            raise AssertionError(f"node {name!r} enables goal_gate — breaks A/A+B symmetry")
        if str(node.attrs.get("retry_target", "")):
            raise AssertionError(f"node {name!r} declares retry_target — breaks A/A+B symmetry")
        if str(node.attrs.get("max_visits", "")) not in ("", "1"):
            raise AssertionError(
                f"node {name!r} has max_visits={node.attrs['max_visits']} — "
                "breaks A/A+B symmetry"
            )
    _assert_no_cycles(graph)


def _run_middle_mode_a(ir: GraphIR, ctx: Context, dot_dir: pathlib.Path) -> tuple[list[dict], bool]:
    """Mode A middle: runner walks start -> middle -> exit. Returns per-node token
    records and whether all middle nodes succeeded (same semantics as Mode A+B ok)."""
    dot_path = dot_dir / "mode_a_middle.dot"
    dot_path.write_text(render_middle_only_dot(ir))
    graph = parser_mod.parse(dot_path)
    assert_no_retry_wiring(graph)
    history = engine_mod.run(graph, ctx, max_steps=100)
    middle_names = {n.name for n in ir.middle_chain()}
    # Keep only the LAST StepRecord per middle node — matches Mode A+B's
    # one-record-per-node semantics. assert_no_retry_wiring prevents max_visits>1
    # and cycles, so there is normally exactly one visit; this dedup is a defensive guard.
    last_by_node: dict[str, object] = {}
    for s in history:
        if s.node in middle_names:
            last_by_node[s.node] = s
    token_records = [
        read_token_record(last_by_node[n.name].metadata)
        for n in ir.middle_chain()
        if n.name in last_by_node
    ]
    # All expected middle nodes must have appeared in history — a missing node
    # (runner stopped early, max_steps exhausted) is treated as a failure, not
    # silently ignored by a vacuous all(). Mode A+B always executes the full chain.
    middle_ok = (
        set(last_by_node.keys()) >= middle_names
        and all(s.outcome in ("success", "warn") for s in last_by_node.values())
    )
    return token_records, middle_ok


def _run_middle_mode_apb(ir: GraphIR, ctx: Context) -> tuple[list[dict], bool]:
    """Mode A+B middle: harness calls `_codergen` per middle node in chain order,
    same claude/Sonnet coder. Returns per-node token records and success flag."""
    token_records: list[dict] = []
    ok = True
    for nir in ir.middle_chain():
        # nir.prompt is documented as no-leading-@ but validate() accepts both forms,
        # so strip any existing @ before prepending to avoid "@@prompts/..." double-prefix.
        prompt_ref = nir.prompt if nir.prompt.startswith("@") else "@" + nir.prompt
        attrs = {"type": nir.type, "prompt": prompt_ref, "label": nir.name}
        if nir.backend:
            attrs["backend"] = nir.backend
        if nir.model_name:
            attrs["model_name"] = nir.model_name
        result = _codergen(Node(name=nir.name, attrs=attrs), ctx)
        token_records.append(read_token_record(result.metadata))
        if result.outcome not in ("success", "warn"):
            ok = False
    return token_records, ok


def _run_spine(ir: GraphIR, ctx: Context, dot_dir: pathlib.Path) -> bool:
    """Run the terminal reviewer spine through the runner (identical for both
    modes). Reviewers are terminal, so this always reaches exit; returns that."""
    dot_path = dot_dir / "spine.dot"
    dot_path.write_text(render_spine_dot(ir))
    graph = parser_mod.parse(dot_path)
    assert_no_retry_wiring(graph)
    history = engine_mod.run(graph, ctx, max_steps=100)
    return any(parser_mod.is_exit_node(graph.nodes[s.node]) for s in history if s.node in graph.nodes)


def run_one(
    *,
    ir: GraphIR,
    mode: str,
    feature: str,
    goal: str,
    workdir: pathlib.Path,
    backend: str,
    model_name: str | None,
    trial: int,
    fit_score: int | None = None,
    holdout_evaluator=None,
    exploratory: bool = False,
    dot_dir: pathlib.Path | None = None,
    run_spine: bool = True,
) -> dict:
    """Run a single (feature, mode, trial) and return one benchmark record.

    ``holdout_evaluator`` is an optional operator-supplied callable
    ``(workdir, feature, baseline_ref, head_ref) -> {pass,total}`` that grades the
    committed diff with the sealed evaluator. When absent, conformance is recorded
    unavailable (the smoke still produces the other three axes).
    """
    # Rendered .dot scaffolding MUST NOT land in the target repo, or `git add -A`
    # would commit it as part of the "middle diff" and confound conformance. Keep
    # it in a throwaway dir outside the workdir unless the caller pins one.
    owns_dot_dir = dot_dir is None
    if owns_dot_dir:
        dot_dir = pathlib.Path(tempfile.mkdtemp(prefix="wfgg-dot-"))
    try:
        return _run_one_inner(
            ir=ir, mode=mode, feature=feature, goal=goal, workdir=workdir,
            backend=backend, model_name=model_name, trial=trial, fit_score=fit_score,
            holdout_evaluator=holdout_evaluator, exploratory=exploratory, dot_dir=dot_dir,
            run_spine=run_spine,
        )
    finally:
        if owns_dot_dir:
            shutil.rmtree(dot_dir, ignore_errors=True)


def _run_one_inner(
    *,
    ir: GraphIR,
    mode: str,
    feature: str,
    goal: str,
    workdir: pathlib.Path,
    backend: str,
    model_name: str | None,
    trial: int,
    fit_score: int | None,
    holdout_evaluator,
    exploratory: bool,
    dot_dir: pathlib.Path,
    run_spine: bool = True,
) -> dict:
    baseline_ref = _head(workdir)
    notes = ""
    t0 = time.monotonic()

    ctx = Context(goal=goal, workdir=workdir, backend=backend, state={})

    if mode == "A":
        token_records, middle_ok = _run_middle_mode_a(ir, ctx, dot_dir)
    elif mode == "A+B":
        token_records, middle_ok = _run_middle_mode_apb(ir, ctx)
    else:
        raise ValueError(f"unknown mode {mode!r}")

    # Commit the combined middle diff onto the baseline ref (both modes).
    _git(workdir, "add", "-A")
    status = _git(workdir, "status", "--porcelain")
    diff_empty = status == ""
    if not diff_empty:
        _git(workdir, "-c", "user.name=wfgg", "-c", "user.email=wfgg@local",
             "commit", "-m", f"wfgg middle {mode} {feature} trial{trial}")
    head_ref = _head(workdir)
    if diff_empty:
        notes = "empty diff against baseline_ref — recorded as failed run"

    # Reviewer spine (identical for both modes) grades the committed diff. The
    # spine is mode-invariant and terminal (unconditional edges to exit), so it
    # cannot separate A from A+B; the core measurement skips it (run_spine=False)
    # to isolate the independent variable (the middle) and make large-n feasible.
    # When skipped, reached_exit_spine is neutral-True (the spine would always
    # have reached exit), so zero_touch reflects the middle + non-empty diff only.
    reached_exit_spine = _run_spine(ir, ctx, dot_dir) if run_spine else True
    spine_ran = run_spine

    # axis 1: conformance via sealed evaluator (operator-run, optional).
    if holdout_evaluator is not None:
        try:
            conf = holdout_evaluator(workdir, feature, baseline_ref, head_ref)
            conformance = {"available": True, **conf}
        except Exception as exc:  # noqa: BLE001 - record, don't crash the smoke
            conformance = {"available": False, "error": str(exc)}
    else:
        conformance = {"available": False, "reason": "no sealed evaluator wired for smoke"}

    # axis 2: tokens — summed over the middle, identical fields both modes.
    tokens = sum_token_records(token_records)
    wall_ms_total = int((time.monotonic() - t0) * 1000)

    # axis 3: graph quality — shared IR, mode-invariant.
    gq = graph_quality(ir, fit_score=fit_score)

    # axis 4: robustness / zero-touch.
    zero_touch = bool(middle_ok and reached_exit_spine and not diff_empty)
    loop_bounds_held = True  # linear smoke middle has no loops
    spine_note = "spine=on" if spine_ran else "spine=off(mode-invariant,isolating middle)"
    notes = f"{notes}; {spine_note}".lstrip("; ") if notes else spine_note

    return assemble_record(
        feature=feature,
        mode=mode,
        trial=trial,
        backend=backend,
        model_name=model_name,
        conformance=conformance,
        tokens=tokens,
        wall_ms_total=wall_ms_total,
        graph_quality_score=gq,
        zero_touch=zero_touch,
        loop_bounds_held=loop_bounds_held,
        baseline_ref=baseline_ref,
        head_ref=head_ref,
        diff_empty=diff_empty,
        exploratory=exploratory,
        notes=notes,
    )


def generate_ir(goal: str, backend: str, model_name: str | None) -> GraphIR:
    """Generate the shared graph-IR once per goal (deterministic default)."""
    ir = deterministic_generator(goal, backend=backend, model_name=model_name)
    ir.validate()
    validate_node_prompts(n.prompt for n in ir.nodes)
    return ir


# Smoke goals — deliberately generic, self-contained coding tasks. They contain
# NO holdout/evaluator content (agent isolation): the coder is told only the
# public behavior to build, exactly as a public spec would state it.
SMOKE_GOALS = {
    "hello": (
        "Create a Python module `hello.py` exposing `def hello(name: str) -> str` "
        "that returns the string 'Hello, <name>!'. For an empty or whitespace-only "
        "name, return 'Hello, World!'. Add a `__main__` block that prints "
        "hello() using sys.argv[1] when given, else the default."
    ),
    "roman": (
        "Create a Python module `roman.py` exposing `def to_roman(n: int) -> str` "
        "that converts an integer in 1..3999 to its Roman-numeral string "
        "(e.g. 4 -> 'IV', 1994 -> 'MCMXCIV'). Raise ValueError for inputs outside "
        "1..3999 or non-integers. Use standard subtractive notation."
    ),
}


def _seed_repo(path: pathlib.Path) -> None:
    """Create a fresh single-commit git repo at ``path`` for one smoke run."""
    path.mkdir(parents=True, exist_ok=True)
    _git(path, "init", "-q")
    _git(path, "config", "user.name", "wfgg")
    _git(path, "config", "user.email", "wfgg@local")
    (path / "README.md").write_text("workflow_graphgen smoke workdir\n")
    _git(path, "add", "-A")
    _git(path, "-c", "user.name=wfgg", "-c", "user.email=wfgg@local", "commit", "-qm", "seed")


def _mode_slug(mode: str) -> str:
    return mode.replace("+", "p")


def run_benchmark(
    *,
    features: list[str],
    modes: list[str],
    trials: int,
    backend: str,
    model_name: str | None,
    workroot: pathlib.Path,
    out_path: pathlib.Path | None = None,
    holdout_evaluator=None,
    run_spine: bool = True,
    exploratory: bool = False,
) -> list[dict]:
    """Run the A vs A+B benchmark over ``features`` x ``modes`` x ``trials``.

    Each (feature, mode, trial) gets its own freshly-seeded workdir so the modes
    never contaminate each other's git state. The graph-IR is generated ONCE per
    goal and shared by both modes (fairness control). Records are appended to
    ``out_path`` as JSONL when given, and also returned.
    """
    records: list[dict] = []
    if out_path is not None:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text("")  # truncate for a clean run
    for feature in features:
        goal = SMOKE_GOALS.get(feature)
        if goal is None:
            raise ValueError(f"no smoke goal defined for feature {feature!r}")
        ir = generate_ir(goal, backend, model_name)  # shared by both modes
        for mode in modes:
            for trial in range(1, trials + 1):
                wd = workroot / f"{feature}_{_mode_slug(mode)}_t{trial}"
                if wd.exists():
                    shutil.rmtree(wd, ignore_errors=True)
                _seed_repo(wd)
                rec = run_one(
                    ir=ir,
                    mode=mode,
                    feature=feature,
                    goal=goal,
                    workdir=wd,
                    backend=backend,
                    model_name=model_name,
                    trial=trial,
                    holdout_evaluator=holdout_evaluator,
                    exploratory=exploratory,
                    run_spine=run_spine,
                )
                records.append(rec)
                if out_path is not None:
                    with out_path.open("a") as fh:
                        fh.write(json.dumps(rec) + "\n")
    return records
