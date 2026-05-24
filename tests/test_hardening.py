"""Regression tests for iteration-2 hardening fixes.

Covers:
  - _parse_verdict marker discipline + standalone fallback
  - CXDB WAL pragma + concurrent-writer survival
  - engine._attr_int defensive default on bad attrs
  - engine.run records runs.ended_ts via try/finally even on stuck pipelines
"""

from __future__ import annotations

import json
import pathlib
import sqlite3
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

import runner.handlers as handlers_mod  # noqa: E402
from runner.cxdb import CXDB  # noqa: E402
from runner.engine import _attr_int, _edge_matches, run  # noqa: E402
from runner.handlers import (  # noqa: E402
    Context,
    Result,
    TYPE_REGISTRY,
    _codergen,
    _holdout_eval,
    _parse_verdict,
    _render_prompt,
    _sanitized_env,
)
from runner.parser import Edge  # noqa: E402
from runner.parser import Node, parse  # noqa: E402


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


# ---------------------------------------------------------------------------
# _parse_verdict
# ---------------------------------------------------------------------------

def test_parse_verdict_ignores_compound_text():
    """A marker line whose value is not a verdict token must not collapse via
    the fallback into the embedded word ('fail' inside 'not a fail')."""
    raw, norm = _parse_verdict("verdict: not a fail")
    assert (raw, norm) != ("fail", "failure"), (
        "marker regex should require verdict:<TOKEN> and the fallback should "
        "not lift the embedded 'fail' out of compound prose"
    )

    raw2, norm2 = _parse_verdict("passes warnings cleanly")
    assert (raw2, norm2) != ("pass", "success"), (
        "no marker present; 'passes' is not a standalone PASS token"
    )


def test_parse_verdict_picks_last_marker():
    """If multiple VERDICT: lines appear, the last one wins."""
    text = "VERDICT: FAIL\nstuff happens\nVERDICT: PASS\n"
    raw, norm = _parse_verdict(text)
    assert raw == "pass"
    assert norm == "success"


def test_parse_verdict_standalone_fallback():
    """No explicit marker but tail contains a bare verdict token on its own line → success."""
    body = "noise line 1\nnoise line 2\nPASS\n"
    raw, norm = _parse_verdict(body)
    assert norm == "success"
    assert raw == "pass"


def test_tool_handler_tolerates_bad_timeout(tmp_path, monkeypatch):
    """`_tool` must not crash when a .dot author writes `timeout="abc"`.

    Also verifies the command actually ran and its output flowed through:
    the previous version of this test only checked the outcome was in a
    set that included both success and failure, so a no-op would have
    passed it silently.
    """
    from runner.handlers import _tool, Context, Result
    from runner.parser import Node

    node = Node(name="t", attrs={"command": "echo hi", "timeout": "not-a-number"})
    ctx = Context(goal="t", workdir=tmp_path, backend="echo")
    result = _tool(node, ctx)
    assert isinstance(result, Result)
    assert result.outcome == "success", f"unexpected outcome: {result.outcome}"
    assert "hi" in result.output, f"command output missing: {result.output!r}"
    assert result.metadata.get("returncode") == "0", result.metadata


def test_parse_verdict_marker_invalid_token_does_not_fall_back():
    """If a `verdict:` marker exists with an invalid token, refuse to guess.

    Prevents "verdict: not a fail" from being misclassified as a fail verdict
    via the standalone fallback grabbing "fail" out of "not a fail".
    """
    raw, norm = _parse_verdict("verdict: not a fail")
    assert raw == "unknown"
    assert norm == "failure"


# ---------------------------------------------------------------------------
# CXDB pragmas + concurrent writes
# ---------------------------------------------------------------------------

def test_cxdb_wal_pragma(tmp_path):
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        mode = db._conn.execute("PRAGMA journal_mode").fetchone()[0]
    finally:
        db.close()
    assert mode.lower() == "wal", f"expected WAL journal_mode, got {mode!r}"


def test_cxdb_concurrent_writes(tmp_path):
    """Two CXDB instances on the same file must both write without
    'database is locked' — the busy_timeout PRAGMA absorbs brief contention."""
    db_path = tmp_path / "cxdb.sqlite"
    db_a = CXDB(db_path)
    db_b = CXDB(db_path)
    try:
        run_a = db_a.start_run(pipeline="p", goal="g")
        run_b = db_b.start_run(pipeline="p", goal="g")
        db_a.record_step(
            run_id=run_a, seq=0, node="n", outcome="success",
            ts=0.0, output="hello", metadata={},
        )
        # If WAL+busy_timeout is missing this raises sqlite3.OperationalError.
        db_b.record_step(
            run_id=run_b, seq=0, node="n", outcome="success",
            ts=0.0, output="world", metadata={},
        )
    finally:
        db_a.close()
        db_b.close()

    # Verify both rows landed.
    conn = sqlite3.connect(str(db_path))
    try:
        n = conn.execute("SELECT COUNT(*) FROM steps").fetchone()[0]
    finally:
        conn.close()
    assert n == 2


# ---------------------------------------------------------------------------
# engine._attr_int defensive default
# ---------------------------------------------------------------------------

def test_attr_int_fallback():
    node = Node(name="x", attrs={"max_visits": "bad"})
    assert _attr_int(node, "max_visits", 0) == 0
    # Empty string also falls back.
    node2 = Node(name="x", attrs={"max_visits": ""})
    assert _attr_int(node2, "max_visits", 7) == 7
    # Missing key falls back.
    assert _attr_int(Node(name="x", attrs={}), "max_visits", 5) == 5
    # Valid int parses normally.
    assert _attr_int(Node(name="x", attrs={"max_visits": "3"}), "max_visits", 0) == 3


def test_malformed_edge_condition_fails_closed():
    edge = Edge(src="a", dst="b", attrs={"condition": "not-a-condition"})
    assert _edge_matches(edge, Result(outcome="success")) is False


# ---------------------------------------------------------------------------
# engine.run finally-block CXDB closure on stuck pipelines
# ---------------------------------------------------------------------------

def test_engine_records_finally_on_stuck(monkeypatch, tmp_path):
    """When the engine hits a 'stuck' state (no outgoing edge matches),
    the run must still be closed: runs.ended_ts non-null."""
    # Holdout returns an outcome that neither edge condition matches —
    # hello.dot only has condition=outcome=success and outcome!=success,
    # so to force "stuck" we patch _pick_next via TYPE_REGISTRY ... actually
    # outcome!=success covers everything-not-success. Easier: emit an outcome
    # that matches *no* edge by making holdout the terminal and removing edges.
    #
    # Cheapest path: use a stand-in handler whose result has no matching edge
    # by patching the registry to mark the FIRST node 'plan' as a type that
    # returns outcome="weird", and craft a pipeline where 'plan' has only a
    # conditional edge that does not match.
    #
    # Use hello.dot but force `implement` to return outcome="weird"; the only
    # outgoing edge implement->holdout is unconditional so it would still match.
    # Trick: hello.dot's holdout has two conditional edges — both branches are
    # covered. So we synthesize a tiny pipeline in tmp_path that has only a
    # conditional outgoing edge from 'plan' that requires outcome=success, and
    # have plan return outcome="weird".

    dot = tmp_path / "stuck.dot"
    dot.write_text(
        'digraph stuck {\n'
        '  graph [goal="stuck" label="stuck"]\n'
        '  start [shape=Mdiamond label="Start"]\n'
        '  exit  [shape=Msquare label="Exit"]\n'
        '  plan  [type="codergen" label="Plan" prompt="@nope.md"]\n'
        '  start -> plan\n'
        '  plan  -> exit [condition="outcome=success"]\n'
        '}\n'
    )

    def weird_plan(node, ctx):
        return Result(outcome="weird", output="no edge matches me")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", weird_plan)

    db_path = tmp_path / "cxdb.sqlite"
    g = parse(dot)
    ctx = Context(goal="t", workdir=ROOT, backend="echo", cxdb_path=db_path)
    history = run(g, ctx, max_steps=10)

    assert any(r.outcome == "stuck" for r in history), \
        f"expected a 'stuck' step, got {[r.outcome for r in history]}"

    # The finally block must have flushed runs.ended_ts even though the loop
    # broke via the stuck branch (not via 'exit').
    conn = sqlite3.connect(str(db_path))
    try:
        row = conn.execute(
            "SELECT ended_ts, final FROM runs"
        ).fetchone()
    finally:
        conn.close()
    assert row is not None, "no run row recorded"
    ended_ts, final = row
    assert ended_ts is not None, "runs.ended_ts is NULL — finally block did not fire"
    assert final == "stuck", f"expected final='stuck', got {final!r}"


def test_checkpoint_includes_synthetic_terminal_record(monkeypatch, tmp_path):
    """Checkpoint state must match in-memory history after max_visits exhaustion."""
    fake_holdout = lambda node, ctx: Result(outcome="fail", output="boom")
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    checkpoint = tmp_path / "checkpoint.json"
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    history = run(g, ctx, checkpoint=checkpoint, max_steps=50)

    saved = json.loads(checkpoint.read_text())
    assert history[-1].outcome == "exhausted"
    assert saved[-1]["outcome"] == "exhausted"
    assert len(saved) == len(history)


def test_prompt_references_cannot_escape_workdir():
    node = Node(
        name="leak",
        attrs={
            "prompt": "@/Users/jleechan/projects/dark-factory-holdouts/holdouts/hello/scenarios.yaml"
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    text = _render_prompt(node, ctx)

    assert "expect_return" not in text
    assert "Hello, world!" not in text
    assert "invalid prompt" in text


def test_sanitized_env_strips_holdout_paths(monkeypatch):
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", "/secret/holdouts")
    monkeypatch.setenv("SOME_HOLDOUT_TOKEN", "secret")
    monkeypatch.setenv("SAFE_VALUE", "ok")

    env = _sanitized_env()

    assert "DARK_FACTORY_HOLDOUTS" not in env
    assert "SOME_HOLDOUT_TOKEN" not in env
    assert env["SAFE_VALUE"] == "ok"


def test_tool_nodes_cannot_read_holdout_files():
    from runner.handlers import _tool

    scenarios = (
        "/Users/jleechan/projects/dark-factory-holdouts/holdouts/hello/scenarios.yaml"
    )
    node = Node(
        name="tool_leak",
        attrs={
            "type": "tool",
            "command": (
                f"{sys.executable} -c "
                f"\"import pathlib; print(pathlib.Path({scenarios!r}).read_text())\""
            ),
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    result = _tool(node, ctx)

    assert result.outcome == "failure"
    assert "expect_return" not in result.output
    assert "Hello, world!" not in result.output


def test_tool_sandbox_still_denies_real_holdouts_when_env_overridden(monkeypatch, tmp_path):
    from runner.handlers import _tool

    fake_holdouts = tmp_path / "fake-holdouts"
    fake_holdouts.mkdir()
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(fake_holdouts))
    scenarios = (
        "/Users/jleechan/projects/dark-factory-holdouts/holdouts/hello/scenarios.yaml"
    )
    node = Node(
        name="tool_leak",
        attrs={
            "type": "tool",
            "command": (
                f"{sys.executable} -c "
                f"\"import pathlib; print(pathlib.Path({scenarios!r}).read_text())\""
            ),
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    result = _tool(node, ctx)

    assert result.outcome == "failure"
    assert "expect_return" not in result.output


def test_ao_spawn_is_launched_through_holdout_sandbox(monkeypatch, tmp_path):
    commands = []
    prompt = tmp_path / "prompt.md"
    prompt.write_text("do work")

    def fake_sandbox(args):
        return ["sandboxed", *args]

    def fake_run(args, **kwargs):
        commands.append(args)

        class Proc:
            returncode = 0
            stdout = "SESSION=session-1\nWorktree: /tmp/ao-worktree\n"
            stderr = ""

        return Proc()

    monkeypatch.setattr(handlers_mod, "_sandboxed_args", fake_sandbox)
    monkeypatch.setattr(handlers_mod.subprocess, "run", fake_run)
    monkeypatch.setattr(handlers_mod, "_ao_wait_idle", lambda *args, **kwargs: "ready")

    ctx = Context(goal="t", workdir=tmp_path, backend="ao", state={"ao.project": "dark-factory"})
    result = _codergen(Node(name="implement", attrs={"prompt": "@prompt.md"}), ctx)

    assert result.outcome == "success"
    assert commands
    assert commands[0][:3] == ["sandboxed", "ao", "spawn"]


def test_ao_send_is_launched_through_holdout_sandbox(monkeypatch, tmp_path):
    commands = []
    prompt = tmp_path / "prompt.md"
    prompt.write_text("fix work")

    def fake_sandbox(args):
        return ["sandboxed", *args]

    def fake_run(args, **kwargs):
        commands.append(args)

        class Proc:
            returncode = 0
            stdout = ""
            stderr = ""

        return Proc()

    monkeypatch.setattr(handlers_mod, "_sandboxed_args", fake_sandbox)
    monkeypatch.setattr(handlers_mod.subprocess, "run", fake_run)
    monkeypatch.setattr(handlers_mod, "_ao_wait_idle", lambda *args, **kwargs: "ready")

    ctx = Context(
        goal="t",
        workdir=tmp_path,
        backend="ao",
        state={"ao.project": "dark-factory", "ao.session": "session-1"},
    )
    result = _codergen(Node(name="fix", attrs={"prompt": "@prompt.md"}), ctx)

    assert result.outcome == "success"
    assert commands
    assert commands[0][:3] == ["sandboxed", "ao", "send"]


def test_ao_backend_fails_closed_without_sandbox(monkeypatch, tmp_path):
    prompt = tmp_path / "prompt.md"
    prompt.write_text("do work")
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: None)

    ctx = Context(goal="t", workdir=tmp_path, backend="ao", state={"ao.project": "dark-factory"})
    result = _codergen(Node(name="implement", attrs={"prompt": "@prompt.md"}), ctx)

    assert result.outcome == "failure"
    assert "sandbox-exec unavailable" in result.output


def test_intermediate_success_does_not_clear_unvalidated_failure(tmp_path):
    dot = tmp_path / "greenwash.dot"
    dot.write_text(
        'digraph greenwash {\n'
        '  start [shape=Mdiamond]\n'
        '  fail [type="tool" command="/usr/bin/false"]\n'
        '  ok [type="codergen"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> fail -> ok -> exit\n'
        '}\n'
    )
    g = parse(dot)
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    history = run(g, ctx)

    assert any(r.node == "fail" and r.outcome == "failure" for r in history)
    assert history[-1].outcome == "failure"


def test_holdout_eval_ignores_pipeline_repo_override(tmp_path):
    fake_repo = tmp_path / "fake-holdouts"
    evaluator = fake_repo / "evaluator"
    evaluator.mkdir(parents=True)
    (evaluator / "run.py").write_text(
        "import json\nprint(json.dumps({'verdict': 'pass', 'scenarios': []}))\n"
    )

    node = Node(
        name="holdout",
        attrs={
            "type": "holdout_eval",
            "feature": "missing-feature-name",
            "holdouts_repo": str(fake_repo),
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    result = _holdout_eval(node, ctx)

    assert result.outcome != "success"


def test_holdout_eval_nonzero_returncode_cannot_spoof_pass(monkeypatch, tmp_path):
    fake_repo = tmp_path / "fake-holdouts"
    evaluator = fake_repo / "evaluator"
    evaluator.mkdir(parents=True)
    (evaluator / "run.py").write_text(
        "import json, sys\nprint(json.dumps({'verdict': 'pass', 'scenarios': []}))\nsys.exit(17)\n"
    )
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(fake_repo))

    node = Node(name="holdout", attrs={"type": "holdout_eval", "feature": "hello"})
    ctx = Context(goal="t", workdir=ROOT, backend="echo")

    result = _holdout_eval(node, ctx)

    assert result.outcome != "success"


def test_holdout_eval_uses_state_substituted_implementation(monkeypatch, tmp_path):
    fake_repo = tmp_path / "fake-holdouts"
    evaluator = fake_repo / "evaluator"
    evaluator.mkdir(parents=True)
    (evaluator / "run.py").write_text(
        "import argparse, json, pathlib\n"
        "p = argparse.ArgumentParser()\n"
        "p.add_argument('--feature')\n"
        "p.add_argument('--implementation')\n"
        "args = p.parse_args()\n"
        "marker = pathlib.Path(args.implementation, 'marker.txt')\n"
        "verdict = 'pass' if marker.exists() else 'fail'\n"
        "print(json.dumps({'verdict': verdict, 'scenarios': []}))\n"
    )
    impl = tmp_path / "worker"
    impl.mkdir()
    (impl / "marker.txt").write_text("ok")
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(fake_repo))

    node = Node(
        name="holdout",
        attrs={
            "type": "holdout_eval",
            "feature": "roman",
            "implementation": "${state.ao.worktree}",
        },
    )
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    ctx.state["ao.worktree"] = str(impl)

    result = _holdout_eval(node, ctx)

    assert result.outcome == "success"


def test_visible_all_nodes_benchmark_has_no_embedded_holdout_contract():
    benchmark = ROOT / "benchmarks" / "all-nodes-coverage"

    assert not (benchmark / "_holdout").exists()
    for path in benchmark.rglob("*"):
        if not path.is_file() or path.name == "README.md":
            continue
        text = path.read_text()
        assert "_holdout" not in text
        assert "cp -R /Users/jleechan/projects/dark-factory/benchmarks" not in text


def test_holdout_eval_fails_closed_on_unresolved_implementation(monkeypatch, tmp_path):
    fake_repo = tmp_path / "fake-holdouts"
    evaluator = fake_repo / "evaluator"
    evaluator.mkdir(parents=True)
    (evaluator / "run.py").write_text(
        "import json\nprint(json.dumps({'verdict': 'pass', 'scenarios': []}))\n"
    )
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(fake_repo))

    node = Node(
        name="holdout",
        attrs={
            "type": "holdout_eval",
            "feature": "roman",
            "implementation": "${state.ao.worktree}",
        },
    )
    ctx = Context(goal="t", workdir=tmp_path, backend="echo")

    result = _holdout_eval(node, ctx)

    assert result.outcome == "failure"
    assert "unresolved implementation path" in result.output


def test_holdout_eval_redacts_scenarios_from_agent_output(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    repo = tmp_path / "sealed"
    evaluator = repo / "evaluator"
    evaluator.mkdir(parents=True)
    (evaluator / "run.py").write_text(
        "import json\n"
        "print(json.dumps({"
        "'verdict': 'fail', "
        "'scenarios': [{'id': 'secret-story', 'status': 'fail', 'detail': 'hidden checkout edge'}]"
        "}))\n"
    )
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(repo))
    impl = tmp_path / "impl"
    impl.mkdir()

    node = Node(name="holdout", attrs={"type": "holdout_eval", "feature": "hello"})
    ctx = Context(goal="t", workdir=impl, backend="echo")

    result = _holdout_eval(node, ctx)

    assert result.outcome == "fail"
    assert "secret-story" not in result.output
    assert "hidden checkout edge" not in result.output
    assert json.loads(result.output)["sealed"] is True


def test_holdout_eval_writes_only_redacted_results_to_impl(monkeypatch, tmp_path):
    monkeypatch.setattr(handlers_mod, "_sandboxed_args", lambda args: args)
    repo = tmp_path / "sealed"
    evaluator = repo / "evaluator"
    evaluator.mkdir(parents=True)
    (evaluator / "run.py").write_text(
        "import json\n"
        "print(json.dumps({"
        "'verdict': 'pass', "
        "'scenarios': [{'id': 'secret-story', 'status': 'pass', 'detail': 'hidden checkout edge'}]"
        "}))\n"
    )
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(repo))
    impl = tmp_path / "impl"
    impl.mkdir()

    node = Node(name="holdout", attrs={"type": "holdout_eval", "feature": "hello"})
    ctx = Context(goal="t", workdir=impl, backend="echo")

    result = _holdout_eval(node, ctx)

    assert result.outcome == "success"
    saved = json.loads((impl / "results" / "holdout_results.json").read_text())
    assert saved == {
        "verdict": "pass",
        "passed": 1,
        "total": 1,
        "status_counts": {"pass": 1},
        "sealed": True,
    }
    assert "secret-story" not in json.dumps(saved)
    assert "hidden checkout edge" not in json.dumps(saved)
