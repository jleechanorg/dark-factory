"""Echo-backend plumbing tests for the workflow_graphgen harness.

Proves the full Mode A / Mode A+B machinery end-to-end without an LLM:
  * a shared graph-IR is generated once and drives both modes,
  * each mode records baseline_ref, runs its middle, (no-)commits, runs the
    shared reviewer spine, and emits a 4-axis record,
  * the only thing that differs between the modes is *who runs the middle*.

Echo writes no files, so diff_empty is True and zero_touch is False by design;
the test asserts the record SHAPE and the fairness invariants, not a real diff.
"""

import json
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from benchmarks.workflow_graphgen import harness  # noqa: E402
from benchmarks.workflow_graphgen.graph_ir import GUARANTEED_NODES  # noqa: E402
from benchmarks.workflow_graphgen.harness import _run_middle_mode_a, _run_middle_mode_apb  # noqa: E402
from runner.handlers import Context  # noqa: E402

_HEX40 = lambda s: isinstance(s, str) and len(s) == 40 and all(c in "0123456789abcdef" for c in s)


def _git(workdir, *args):
    subprocess.run(["git", "-C", str(workdir), *args], check=True, capture_output=True, text=True)


@pytest.fixture
def repo(tmp_path, monkeypatch):
    monkeypatch.setenv("DARK_FACTORY_HOME", str(ROOT))
    wd = tmp_path / "repo"
    wd.mkdir()
    _git(wd, "init", "-q")
    _git(wd, "config", "user.name", "test")
    _git(wd, "config", "user.email", "test@local")
    (wd / "README.md").write_text("seed\n")
    _git(wd, "add", "-A")
    _git(wd, "commit", "-qm", "seed")
    return wd


def _run(mode, repo):
    ir = harness.generate_ir("smoke goal", backend="echo", model_name=None)
    return harness.run_one(
        ir=ir,
        mode=mode,
        feature="hello",
        goal="smoke goal",
        workdir=repo,
        backend="echo",
        model_name=None,
        trial=1,
    )


def test_mode_a_record_shape(repo):
    rec = _run("A", repo)
    assert rec["mode"] == "A"
    assert rec["feature"] == "hello"
    # 4 axes present
    assert "conformance" in rec and rec["conformance"]["available"] is False
    assert set(rec["tokens"]) == {"tokens_in", "tokens_out", "wall_ms"}
    assert "graph_quality" in rec
    assert "zero_touch" in rec
    # provenance
    assert _HEX40(rec["baseline_ref"]) and _HEX40(rec["head_ref"])
    # echo writes nothing -> empty diff -> not zero-touch
    assert rec["diff_empty"] is True
    assert rec["zero_touch"] is False


def test_mode_a_plus_b_record_shape(repo):
    rec = _run("A+B", repo)
    assert rec["mode"] == "A+B"
    assert set(rec["tokens"]) == {"tokens_in", "tokens_out", "wall_ms"}
    assert _HEX40(rec["baseline_ref"]) and _HEX40(rec["head_ref"])
    assert rec["diff_empty"] is True


def test_graph_quality_is_mode_invariant_and_full_deterministic(repo):
    a = _run("A", repo)
    b = _run("A+B", repo)
    # The shared IR is generated identically; deterministic-70 must match.
    assert a["graph_quality"]["deterministic_70"] == b["graph_quality"]["deterministic_70"]
    # Both reviewer nodes present + graph renders valid -> full 70.
    assert a["graph_quality"]["presence"] is True
    assert a["graph_quality"]["edge_validity"] is True
    assert a["graph_quality"]["deterministic_70"] == 70.0
    # No fit score wired in the smoke -> unscored partial, never a silent zero.
    assert a["graph_quality"]["unscored"] is True
    assert a["graph_quality"]["score"] is None


def test_fairness_no_retry_wiring_on_rendered_graphs(repo):
    # Both the middle-only (Mode A) and the spine graphs must pass the
    # no-goal_gate / no-retry_target assertion; run_one would raise otherwise.
    rec = _run("A", repo)
    assert rec is not None  # assert_no_retry_wiring did not raise inside run_one


def test_record_json_serializable(repo):
    rec = _run("A+B", repo)
    json.dumps(rec)  # must not raise — records are emitted as JSONL
    assert set(GUARANTEED_NODES) == {"code_reviewer", "gate_es", "gate_er"}


def test_mode_a_zero_touch_not_credited_on_middle_failure(repo, tmp_path):
    """Mode A must not credit middle_ok=True when a middle coder node fails.

    Regression guard: Mode A uses unconditional edges so the runner visits exit
    even after a failed plan/implement.  The old reached_exit approach would return
    True; the outcome-based check must return False, matching Mode A+B semantics.
    """
    monkeypatch_env = {"DARK_FACTORY_HOME": str(ROOT)}
    import os
    saved = {k: os.environ.get(k) for k in monkeypatch_env}
    os.environ.update(monkeypatch_env)
    try:
        ir = harness.generate_ir("smoke goal", backend="echo", model_name=None)
        first_middle = ir.middle_chain()[0]
        ctx = Context(
            goal="smoke goal",
            workdir=repo,
            backend="echo",
            state={f"{first_middle.name}.outcome": "failure"},
        )
        dot_dir = tmp_path / "dots"
        dot_dir.mkdir()
        _, middle_ok = _run_middle_mode_a(ir, ctx, dot_dir)
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
    assert middle_ok is False, "Mode A must propagate node failure to middle_ok"


def test_mode_apb_middle_ok_false_on_failure(repo):
    """Mode A+B ok must be False when any middle node fails (baseline for symmetry)."""
    import os
    os.environ.setdefault("DARK_FACTORY_HOME", str(ROOT))
    ir = harness.generate_ir("smoke goal", backend="echo", model_name=None)
    first_middle = ir.middle_chain()[0]
    ctx = Context(
        goal="smoke goal",
        workdir=repo,
        backend="echo",
        state={f"{first_middle.name}.outcome": "failure"},
    )
    _, ok = _run_middle_mode_apb(ir, ctx)
    assert ok is False


# ── Regression guards for cursor review comments (PR #16) ──────────────────


def test_assert_no_retry_wiring_rejects_max_visits_gt_1():
    """assert_no_retry_wiring must reject nodes with max_visits>1.

    When a middle node carries max_visits>1 the runner may visit it multiple
    times, inflating Mode A's token totals and breaking A/A+B symmetry.
    """
    from runner.parser import Edge, Graph, Node
    graph = Graph(
        name="test", goal="",
        nodes={
            "start": Node("start"),
            "implement": Node("implement", attrs={"max_visits": "3"}),
            "exit": Node("exit"),
        },
        edges=[Edge("start", "implement"), Edge("implement", "exit")],
    )
    with pytest.raises(AssertionError, match="max_visits"):
        harness.assert_no_retry_wiring(graph)


def test_assert_no_retry_wiring_rejects_cycles():
    """assert_no_retry_wiring must detect cyclic fix-loops.

    Mode A's runner follows cyclic edges (plan -> fix -> plan), revisiting
    nodes, while Mode A+B iterates middle_chain() once per node — the two
    modes would execute different numbers of codergen calls, breaking symmetry.
    """
    from runner.parser import Edge, Graph, Node
    graph = Graph(
        name="test", goal="",
        nodes={
            "start": Node("start"),
            "plan": Node("plan"),
            "fix": Node("fix"),
            "exit": Node("exit"),
        },
        edges=[
            Edge("start", "plan"),
            Edge("plan", "fix"),
            Edge("fix", "plan"),  # cycle: fix -> plan
            Edge("plan", "exit"),
        ],
    )
    with pytest.raises(AssertionError, match="cycle"):
        harness.assert_no_retry_wiring(graph)


def test_mode_a_token_records_deduped_by_node(repo, tmp_path):
    """Mode A token_records must contain exactly one record per middle node.

    Defensive guard: even if a node appears twice in engine history (e.g. due
    to a race or bug), the harness must not double-count its tokens.
    """
    import os
    os.environ.setdefault("DARK_FACTORY_HOME", str(ROOT))
    ir = harness.generate_ir("smoke goal", backend="echo", model_name=None)
    ctx = Context(goal="smoke goal", workdir=repo, backend="echo", state={})
    dot_dir = tmp_path / "dots"
    dot_dir.mkdir()
    token_records, _ = _run_middle_mode_a(ir, ctx, dot_dir)
    # One record per unique middle node — no duplicates.
    middle_names = [n.name for n in ir.middle_chain()]
    assert len(token_records) == len(middle_names)


def test_mode_a_token_records_zero_padded_for_missing_nodes(repo, tmp_path, monkeypatch):
    """Mode A token_records must have len(middle_chain) entries even when a node
    was never visited (e.g. runner hit max_steps before reaching it).

    Bugbot #3365197802: omitting missing nodes from token_records lets Mode A
    report artificially low tokens_total in failed runs while Mode A+B always
    charges one record per middle_chain() node — asymmetric cost accounting.
    Missing nodes must be padded with a zero-record so both modes charge the
    same number of entries regardless of execution completeness.
    """
    import os
    os.environ.setdefault("DARK_FACTORY_HOME", str(ROOT))
    ir = harness.generate_ir("smoke goal", backend="echo", model_name=None)
    middle_names = [n.name for n in ir.middle_chain()]
    assert len(middle_names) >= 2, "need ≥2 middle nodes to drop one"

    import runner.engine as engine_mod_real
    original_run = engine_mod_real.run

    def _run_drop_last(*args, **kwargs):
        full = original_run(*args, **kwargs)
        last_middle = ir.middle_chain()[-1].name
        return [s for s in full if s.node != last_middle]

    monkeypatch.setattr(engine_mod_real, "run", _run_drop_last)

    ctx = Context(goal="smoke goal", workdir=repo, backend="echo", state={})
    dot_dir = tmp_path / "dots"
    dot_dir.mkdir()
    token_records, middle_ok = _run_middle_mode_a(ir, ctx, dot_dir)

    assert middle_ok is False, "missing node must make middle_ok=False"
    # CRITICAL: token_records must still have one entry per middle node.
    assert len(token_records) == len(middle_names), (
        f"expected {len(middle_names)} token records (zero-padded), got {len(token_records)}"
    )


def test_run_benchmark_passes_exploratory_flag_through_to_records(tmp_path, monkeypatch):
    """run_benchmark must accept and forward exploratory=True to each run_one call.

    Cursor Bugbot #3365141196 — run_benchmark always hardcodes exploratory=False,
    so exploratory runs via run_benchmark are silently mis-labelled in the output
    records and contaminate downstream aggregation.
    """
    import os
    os.environ.setdefault("DARK_FACTORY_HOME", str(ROOT))
    captured: list[dict] = []

    def fake_run_one(**kwargs):
        captured.append(kwargs)
        return {
            "feature": kwargs["feature"],
            "mode": kwargs["mode"],
            "trial": kwargs["trial"],
            "exploratory": kwargs.get("exploratory", False),
            "tokens": {"tokens_in": 0, "tokens_out": 0, "wall_ms": 0},
            "graph_quality": {
                "score": None, "unscored": True, "presence": True,
                "edge_validity": True, "deterministic_70": 70.0,
            },
            "zero_touch": False,
            "conformance": {"available": False},
            "baseline_ref": "a" * 40,
            "head_ref": "b" * 40,
            "diff_empty": True,
            "loop_bounds_held": True,
            "backend": "echo",
            "model_name": None,
            "notes": "",
        }

    monkeypatch.setattr(harness, "_seed_repo", lambda p: None)
    monkeypatch.setattr(harness, "run_one", fake_run_one)

    records = harness.run_benchmark(
        features=["hello"],
        modes=["A"],
        trials=1,
        backend="echo",
        model_name=None,
        workroot=tmp_path,
        exploratory=True,
    )
    assert len(captured) == 1, "run_benchmark should have called run_one once"
    assert captured[0]["exploratory"] is True, (
        "run_benchmark must forward exploratory=True to run_one"
    )
    assert records[0]["exploratory"] is True


def test_mode_a_unvisited_middle_node_means_failure(repo, tmp_path, monkeypatch):
    """Mode A middle_ok must be False when any expected middle node was never visited.

    If the runner stops early (e.g. max_steps exhausted) before visiting every
    middle node, the missing node must count as a failure — not be silently ignored
    by an empty all() returning True.  Mode A+B always executes the full chain, so
    the two modes must agree on what 'incomplete execution' means.
    """
    import os
    os.environ.setdefault("DARK_FACTORY_HOME", str(ROOT))
    ir = harness.generate_ir("smoke goal", backend="echo", model_name=None)
    middle_names = {n.name for n in ir.middle_chain()}
    assert len(middle_names) >= 1, "IR must have at least one middle node"

    import runner.engine as engine_mod_real
    original_run = engine_mod_real.run

    def _run_missing_last(*args, **kwargs):
        full = original_run(*args, **kwargs)
        # Drop ALL steps for the last middle node to simulate it never running.
        last_middle = ir.middle_chain()[-1].name
        return [s for s in full if s.node != last_middle]

    monkeypatch.setattr(engine_mod_real, "run", _run_missing_last)

    ctx = Context(goal="smoke goal", workdir=repo, backend="echo", state={})
    dot_dir = tmp_path / "dots"
    dot_dir.mkdir()
    _, middle_ok = _run_middle_mode_a(ir, ctx, dot_dir)
    assert middle_ok is False, "missing middle node must make middle_ok=False"
