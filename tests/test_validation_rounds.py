"""Behavioral contract tests for sequential validation rounds."""

from __future__ import annotations

import json
import pathlib

import pytest

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse


def _round_graph(tmp_path: pathlib.Path):
    path = tmp_path / "rounds.dot"
    path.write_text(
        """digraph rounds {
  graph [validation_rounds="true"]
  start [shape=Mdiamond]
  round_begin [type="round_begin", members="check_a,check_b"]
  check_a [type="tool"]
  check_b [type="tool"]
  round_end [type="round_end", members="check_a,check_b"]
  fix [type="tool", max_retries="0"]
  exit [shape=Msquare]
  start -> round_begin -> check_a -> check_b -> round_end
  round_end -> exit [condition="outcome=success"]
  round_end -> fix [condition="outcome!=success"]
  fix -> round_begin
}
"""
    )
    return parse(path)


def _run_rounds(monkeypatch, tmp_path, outcomes, *, max_rounds=3):
    calls: list[str] = []

    def fake_member(node, ctx):
        calls.append(node.name)
        value = outcomes.get(node.name, "success")
        if isinstance(value, list):
            value = value.pop(0)
        return Result(outcome=value, output=f"{node.name}: {value}")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_member)
    ctx = Context(goal="round test", workdir=tmp_path, backend="echo")
    history = run(_round_graph(tmp_path), ctx, max_steps=100, max_rounds=max_rounds)
    return history, calls, ctx


def test_round_runs_all_members_after_one_member_fails(monkeypatch, tmp_path):
    history, calls, _ = _run_rounds(
        monkeypatch,
        tmp_path,
        {"check_a": ["failure", "success"], "check_b": "success", "fix": "success"},
    )

    assert calls[:2] == ["check_a", "check_b"]
    assert calls.count("fix") == 1
    assert history[-1].outcome == "success"


def test_round_continues_after_error_and_aggregates_mixed_results(monkeypatch, tmp_path):
    history, calls, _ = _run_rounds(
        monkeypatch,
        tmp_path,
        {"check_a": "error", "check_b": "failure", "fix": "success"},
        max_rounds=1,
    )

    assert calls[:2] == ["check_a", "check_b"]
    assert calls.count("fix") == 0
    assert history[-1].outcome == "exhausted"


def test_round_recovery_on_second_round_uses_one_fix(monkeypatch, tmp_path):
    history, calls, _ = _run_rounds(
        monkeypatch,
        tmp_path,
        {
            "check_a": ["failure", "success"],
            "check_b": ["failure", "success"],
            "fix": "success",
        },
    )

    assert calls == ["check_a", "check_b", "fix", "check_a", "check_b"]
    assert history[-1].outcome == "success"


def test_three_failed_rounds_have_exactly_two_fixes(monkeypatch, tmp_path):
    history, calls, ctx = _run_rounds(
        monkeypatch,
        tmp_path,
        {"check_a": "failure", "check_b": "failure", "fix": "success"},
    )

    assert calls.count("check_a") == 3
    assert calls.count("check_b") == 3
    assert calls.count("fix") == 2
    assert history[-1].outcome == "exhausted"
    assert ctx.state.get("rounds.requested") == 3


def test_max_rounds_override_changes_bound(monkeypatch, tmp_path):
    history, calls, ctx = _run_rounds(
        monkeypatch,
        tmp_path,
        {"check_a": "failure", "check_b": "failure", "fix": "success"},
        max_rounds=1,
    )

    assert calls == ["check_a", "check_b"]
    assert history[-1].outcome == "exhausted"
    assert ctx.state.get("rounds.requested") == 1


def test_round_gate_error_continuation(monkeypatch, tmp_path):
    """When a member raises an unhandled exception, subsequent members still run."""
    calls: list[str] = []

    def fake_member(node, ctx):
        calls.append(node.name)
        if node.name == "check_a":
            raise RuntimeError("crash inside member check_a")
        return Result(outcome="success", output=f"{node.name}: success")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_member)
    ctx = Context(goal="round test error continuation", workdir=tmp_path, backend="echo")
    history = run(_round_graph(tmp_path), ctx, max_steps=100, max_rounds=1)

    assert calls[:2] == ["check_a", "check_b"]
    assert history[-1].outcome == "exhausted"


def test_round_resets_stale_member_state(monkeypatch, tmp_path):
    """round_begin cleans prior round's member-specific state and test output keys."""
    recorded_states: list[dict] = []

    def fake_member(node, ctx):
        if node.name == "check_a":
            recorded_states.append(
                {
                    "round": ctx.state.get("rounds.current"),
                    "stale_check_a": "check_a.outcome" in ctx.state,
                    "stale_check_b": "check_b.outcome" in ctx.state,
                    "stale_test_output": "last_test_output" in ctx.state,
                }
            )
            # Seed state on round 1
            ctx.state["last_test_output"] = "stale test failure output"
            return Result(outcome="failure", output="check_a failed")
        return Result(outcome="success", output="check_b passed")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_member)
    ctx = Context(goal="stale state test", workdir=tmp_path, backend="echo")
    run(_round_graph(tmp_path), ctx, max_steps=100, max_rounds=2)

    # In round 2, before check_a runs, stale state from round 1 was cleared
    assert len(recorded_states) == 2
    assert recorded_states[0]["round"] == 1
    assert recorded_states[1]["round"] == 2
    assert recorded_states[1]["stale_check_a"] is False
    assert recorded_states[1]["stale_check_b"] is False
    assert recorded_states[1]["stale_test_output"] is False


@pytest.mark.parametrize("bad", ["0", "-1", "-5", "nope", ""])
def test_cli_rejects_invalid_max_rounds(bad):
    """argparse rejects nonpositive or noninteger --max-rounds with exit code 2."""
    from runner.__main__ import main

    with pytest.raises(SystemExit) as exc_info:
        main(["--max-rounds", bad, "--goal", "test goal"])
    assert exc_info.value.code == 2


def test_cli_rejects_missing_max_rounds_value():
    """argparse rejects bare --max-rounds without value with exit code 2."""
    from runner.__main__ import main

    with pytest.raises(SystemExit) as exc_info:
        main(["--max-rounds", "--goal", "test goal"])
    assert exc_info.value.code == 2


def test_cli_propagates_max_rounds_to_context_and_summary(monkeypatch, tmp_path, capsys):
    """Valid positive --max-rounds propagates to engine run and outputs metadata."""
    from runner.__main__ import main

    def fake_handler(node, ctx):
        return Result(outcome="success", output="ok")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_handler)
    monkeypatch.setitem(TYPE_REGISTRY, "parallel_reviewer", fake_handler)

    rc = main(
        [
            "--pipeline",
            "pipelines/slim/two_node.dot",
            "--goal",
            "cli rounds test",
            "--backend",
            "echo",
            "--workdir",
            str(tmp_path),
            "--max-rounds",
            "5",
        ]
    )
    assert rc == 0
    captured = capsys.readouterr()
    summary = json.loads(captured.out)
    assert summary["rounds_requested"] == 5


def test_two_node_validation_rounds_three_failed_rounds_exhausts_without_fourth_worker(
    monkeypatch, tmp_path
):
    """Migrated two_node.dot runs initial worker, then 3 reviewer rounds with exactly 2 fixes (3 worker calls total)."""
    calls: list[str] = []

    def fake_worker(node, ctx):
        calls.append("worker")
        return Result(outcome="success", output="worker output")

    def fake_reviewer(node, ctx):
        calls.append("cold_reviewer")
        return Result(outcome="failure", output="cold review failed")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_worker)
    monkeypatch.setitem(TYPE_REGISTRY, "parallel_reviewer", fake_reviewer)

    graph = parse(pathlib.Path(__file__).parent.parent / "pipelines/slim/two_node.dot")
    ctx = Context(goal="two_node test", workdir=tmp_path, backend="echo")
    history = run(graph, ctx, max_steps=100, max_rounds=3)

    # Initial worker (1) + 3 reviewer rounds + 2 fix worker visits = 3 worker calls, 3 reviewer calls
    assert calls.count("worker") == 3
    assert calls.count("cold_reviewer") == 3
    assert calls == [
        "worker",
        "cold_reviewer",
        "worker",
        "cold_reviewer",
        "worker",
        "cold_reviewer",
    ]
    assert history[-1].outcome == "exhausted"


def test_two_node_validation_rounds_success_exits_early(monkeypatch, tmp_path):
    """Migrated two_node.dot exits on success during round 1 with exactly one worker call."""
    calls: list[str] = []

    def fake_worker(node, ctx):
        calls.append("worker")
        return Result(outcome="success", output="worker output")

    def fake_reviewer(node, ctx):
        calls.append("cold_reviewer")
        return Result(outcome="success", output="cold review passed")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_worker)
    monkeypatch.setitem(TYPE_REGISTRY, "parallel_reviewer", fake_reviewer)

    graph = parse(pathlib.Path(__file__).parent.parent / "pipelines/slim/two_node.dot")
    ctx = Context(goal="two_node green test", workdir=tmp_path, backend="echo")
    history = run(graph, ctx, max_steps=100, max_rounds=3)

    assert calls == ["worker", "cold_reviewer"]
    assert history[-1].outcome == "success"


def test_legacy_graph_retains_short_circuit_routing(monkeypatch, tmp_path):
    """Non-opt-in legacy graphs retain their immediate failure short-circuit routing."""
    legacy_dot = tmp_path / "legacy.dot"
    legacy_dot.write_text(
        """digraph legacy {
  start [shape=Mdiamond]
  gate_1 [type="tool", goal_gate=true, retry_target="fix"]
  gate_2 [type="tool"]
  fix [type="tool", max_visits="1"]
  exit [shape=Msquare]

  start -> gate_1
  gate_1 -> gate_2 [condition="outcome=success"]
  gate_1 -> fix [condition="outcome!=success"]
  gate_2 -> exit [condition="outcome=success"]
  gate_2 -> fix [condition="outcome!=success"]
  fix -> exit
}
"""
    )
    calls: list[str] = []

    def fake_tool(node, ctx):
        calls.append(node.name)
        if node.name == "gate_1":
            return Result(outcome="failure", output="gate 1 failed")
        return Result(outcome="success", output="ok")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_tool)
    graph = parse(legacy_dot)
    ctx = Context(goal="legacy test", workdir=tmp_path, backend="echo")
    history = run(graph, ctx, max_steps=50)

    # In legacy non-opt-in graph, gate_1 failure short-circuits directly to fix without running gate_2
    assert "gate_1" in calls
    assert "gate_2" not in calls
    assert "fix" in calls


def test_pipeline_alias_ready_resolution_precedence(tmp_path, monkeypatch):
    """--pipeline ready resolves to pipelines/slim/ready.dot with local/target-repo precedence."""
    from runner.paths import resolve_pipeline_path

    # Resolves to repo pipelines/slim/ready.dot by default
    resolved = resolve_pipeline_path("ready")
    assert resolved.name == "ready.dot"
    assert "pipelines/slim" in str(resolved)

    # Target repo's dark-factory/pipelines/ready.dot takes precedence
    target_repo = tmp_path / "target_repo"
    subdir = target_repo / "dark-factory" / "pipelines"
    subdir.mkdir(parents=True)
    custom_ready = subdir / "ready.dot"
    custom_ready.write_text("digraph custom {}")

    monkeypatch.chdir(target_repo)
    resolved_custom = resolve_pipeline_path("ready", workdir=target_repo)
    assert resolved_custom == custom_ready.resolve()


def test_ready_runs_all_five_members_for_each_failed_round_even_with_identical_fix_output(
    monkeypatch, tmp_path
):
    """Round budget, rather than fix output repetition, bounds validation rounds."""
    from runner.handlers import _codergen

    calls: list[str] = []

    def always_fail(node, ctx):
        calls.append(node.name)
        return Result(outcome="failure", output="identical failure")

    def always_fix(node, ctx):
        calls.append(node.name)
        return Result(outcome="success", output="identical fix")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", always_fail)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_es", always_fail)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_er", always_fail)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_slash", always_fail)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", always_fail)
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", always_fix)
    graph = parse(pathlib.Path("pipelines/slim/ready.dot"))
    ctx = Context(
        goal="ready rounds",
        workdir=tmp_path,
        backend="echo",
        state={"feature": "ready_feature", "slim.test_command": "true"},
    )

    history = run(graph, ctx, max_steps=100, max_rounds=3)

    assert history[-1].outcome == "exhausted"
    assert calls.count("fix") == 2
    for member in ("test", "gate_es", "gate_er", "gate_advice", "holdout"):
        assert calls.count(member) == 3


def test_member_exception_is_recorded_as_error_in_round_ledger(monkeypatch, tmp_path):
    def fake_member(node, ctx):
        if node.name == "check_a":
            raise RuntimeError("check_a crashed")
        return Result(outcome="success", output="check_b passed")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_member)
    ctx = Context(goal="ledger error", workdir=tmp_path, backend="echo")
    history = run(_round_graph(tmp_path), ctx, max_steps=100, max_rounds=1)

    round_end = next(step for step in history if step.node == "round_end")
    member_outcomes = json.loads(round_end.metadata["member_outcomes"])
    assert member_outcomes == {"check_a": "error", "check_b": "success"}


def test_runner_owned_round_state_override_is_rejected(monkeypatch):
    from runner.__main__ import main

    with pytest.raises(SystemExit) as exc_info:
        main(
            [
                "--pipeline",
                "two_node",
                "--goal",
                "reject reserved state",
                "--backend",
                "echo",
                "--state",
                "rounds.requested=0",
            ]
        )
    assert exc_info.value.code == 2


def test_max_rounds_is_authoritative_over_preseeded_round_state(monkeypatch, tmp_path):
    calls: list[str] = []

    def fake_member(node, ctx):
        calls.append(node.name)
        return Result(outcome="failure", output="failure")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_member)
    ctx = Context(
        goal="authoritative rounds",
        workdir=tmp_path,
        backend="echo",
        state={"rounds.requested": "1"},
    )
    history = run(_round_graph(tmp_path), ctx, max_steps=100, max_rounds=3)

    assert history[-1].outcome == "exhausted"
    assert calls.count("check_a") == 3
    assert ctx.state["rounds.requested"] == 3


def test_terminal_round_end_exhausted_resume_returns_history_without_extra_run(
    monkeypatch, tmp_path
):
    """Resuming a checkpoint ending in terminal round_end exhausted returns history immediately."""
    calls: list[str] = []

    def fake_member(node, ctx):
        calls.append(node.name)
        return Result(outcome="success", output="should not be called")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_member)

    checkpoint = tmp_path / "checkpoint.json"
    checkpoint.write_text(
        json.dumps(
            [
                {"node": "start", "outcome": "success", "ts": 1.0, "output_preview": ""},
                {"node": "round_begin", "outcome": "success", "ts": 2.0, "output_preview": "", "metadata": {"round": "3", "max_rounds": "3", "members": "check_a,check_b"}},
                {"node": "check_a", "outcome": "failure", "ts": 3.0, "output_preview": "fail a"},
                {"node": "check_b", "outcome": "failure", "ts": 4.0, "output_preview": "fail b"},
                {"node": "round_end", "outcome": "exhausted", "ts": 5.0, "output_preview": "exhausted", "metadata": {"aggregate": "exhausted", "exhausted": "true"}},
            ]
        )
    )

    ctx = Context(goal="resume terminal", workdir=tmp_path, backend="echo")
    history = run(_round_graph(tmp_path), ctx, resume=checkpoint, max_steps=100)

    assert len(history) == 5
    assert history[-1].node == "round_end"
    assert history[-1].outcome == "exhausted"
    assert calls == []


def test_mid_round_resume_reconstructs_ledger_without_unknown_members(
    monkeypatch, tmp_path
):
    """Mid-round resume reconstructs prior member outcomes from checkpoint history."""
    calls: list[str] = []

    def fake_member(node, ctx):
        calls.append(node.name)
        return Result(outcome="failure", output="check_b failed")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_member)

    checkpoint = tmp_path / "checkpoint.json"
    checkpoint.write_text(
        json.dumps(
            [
                {"node": "start", "outcome": "success", "ts": 1.0, "output_preview": ""},
                {
                    "node": "round_begin",
                    "outcome": "success",
                    "ts": 2.0,
                    "output_preview": "",
                    "metadata": {"round": "1", "max_rounds": "2", "members": "check_a,check_b"},
                },
                {
                    "node": "check_a",
                    "outcome": "success",
                    "ts": 3.0,
                    "output_preview": "check_a passed",
                    "metadata": {},
                },
            ]
        )
    )

    ctx = Context(goal="resume mid-round", workdir=tmp_path, backend="echo")
    history = run(_round_graph(tmp_path), ctx, resume=checkpoint, max_steps=100, max_rounds=2)

    assert calls[0] == "check_b"  # check_a in round 1 was restored from checkpoint
    round_end_step = next(s for s in history if s.node == "round_end")
    member_outcomes = json.loads(round_end_step.metadata["member_outcomes"])
    assert member_outcomes == {"check_a": "success", "check_b": "failure"}


def test_resume_uses_run_max_rounds_not_checkpoint_round_bound(monkeypatch, tmp_path):
    """A checkpoint's historical bound cannot reduce the current run bound."""
    calls: list[str] = []

    def fake_member(node, ctx):
        calls.append(node.name)
        if node.name == "check_b" and len(calls) == 1:
            return Result(outcome="failure", output="check_b failed")
        return Result(outcome="success", output=f"{node.name} passed")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_member)
    checkpoint = tmp_path / "checkpoint.json"
    checkpoint.write_text(
        json.dumps(
            [
                {"node": "start", "outcome": "success", "ts": 1.0, "output_preview": ""},
                {
                    "node": "round_begin",
                    "outcome": "success",
                    "ts": 2.0,
                    "output_preview": "",
                    "metadata": {
                        "round": "1",
                        "max_rounds": "1",
                        "members": "check_a,check_b",
                    },
                },
                {
                    "node": "check_a",
                    "outcome": "failure",
                    "ts": 3.0,
                    "output_preview": "check_a failed",
                    "metadata": {},
                },
            ]
        )
    )

    ctx = Context(goal="resume authoritative bound", workdir=tmp_path, backend="echo")
    history = run(_round_graph(tmp_path), ctx, resume=checkpoint, max_steps=100, max_rounds=3)

    assert history[-1].outcome == "success"
    assert calls == ["check_b", "fix", "check_a", "check_b"]
    assert ctx.state["rounds.requested"] == 3


def test_cli_resume_from_manifest_preserves_requested_max_rounds(
    monkeypatch, tmp_path, capsys
):
    """CLI resume loads and respects max_rounds from run manifest."""
    from runner.__main__ import main

    run_id = "testrounds12"
    runs_dir = tmp_path / ".dark-factory" / "runs" / run_id
    runs_dir.mkdir(parents=True)
    manifest_file = runs_dir / "manifest.json"
    manifest_file.write_text(
        json.dumps(
            {
                "run_id": run_id,
                "pipeline": "pipelines/slim/ready.dot",
                "goal": "manifest resume test",
                "backend": "echo",
                "workdir": str(tmp_path),
                "max_rounds": 5,
                "rounds_requested": 5,
                "state": {
                    "feature": "test_feat",
                    "slim.test_command": "true",
                },
            }
        )
    )
    checkpoint_file = runs_dir / "checkpoint.json"
    checkpoint_file.write_text(
        json.dumps(
            [
                {"node": "start", "outcome": "success", "ts": 1.0, "output_preview": ""},
                {
                    "node": "round_begin",
                    "outcome": "success",
                    "ts": 2.0,
                    "output_preview": "",
                    "metadata": {"round": "1", "max_rounds": "5", "members": "test,gate_es,gate_er,gate_advice,holdout"},
                },
            ]
        )
    )

    def fake_handler(node, ctx):
        return Result(outcome="success", output="ok")

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_handler)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_es", fake_handler)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_er", fake_handler)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_slash", fake_handler)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_handler)
    monkeypatch.setattr(pathlib.Path, "home", lambda: tmp_path)

    rc = main(["--resume", run_id])
    assert rc == 0
    captured = capsys.readouterr()
    summary = json.loads(captured.out)
    assert summary["rounds_requested"] == 5


def test_two_node_round_ledger_records_both_worker_and_reviewer(
    monkeypatch, tmp_path
):
    """two_node round ledger captures outcomes for both worker and cold_reviewer."""
    calls: list[str] = []

    def fake_worker(node, ctx):
        calls.append("worker")
        return Result(outcome="failure", output="worker error")

    def fake_reviewer(node, ctx):
        calls.append("cold_reviewer")
        return Result(outcome="success", output="reviewer passed")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_worker)
    monkeypatch.setitem(TYPE_REGISTRY, "parallel_reviewer", fake_reviewer)

    graph = parse(pathlib.Path("pipelines/slim/two_node.dot"))
    ctx = Context(goal="two_node ledger test", workdir=tmp_path, backend="echo")
    history = run(graph, ctx, max_steps=100, max_rounds=1)

    round_end_step = next(s for s in history if s.node == "round_end")
    member_outcomes = json.loads(round_end_step.metadata["member_outcomes"])
    assert member_outcomes == {"worker": "failure", "cold_reviewer": "success"}
    assert history[-1].outcome == "exhausted"
