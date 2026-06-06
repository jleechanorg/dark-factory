"""Echo-backend plumbing test for the workflow_graphgen harness.

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
