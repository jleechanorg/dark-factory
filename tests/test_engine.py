"""Smoke tests for parser + engine.

Run with: source .venv/bin/activate && python -m pytest tests/
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline  # noqa: E402

from runner.engine import run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.parser import parse  # noqa: E402


def test_run_captures_target_provenance_before_first_node(tmp_path, monkeypatch):
    subprocess.run(["/usr/bin/git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(["/usr/bin/git", "config", "user.email", "jleechan2015@users.noreply.github.com"], cwd=tmp_path, check=True)
    subprocess.run(["/usr/bin/git", "config", "user.name", "Test"], cwd=tmp_path, check=True)
    (tmp_path / "tracked.txt").write_text("initial\n", encoding="utf-8")
    subprocess.run(["/usr/bin/git", "add", "tracked.txt"], cwd=tmp_path, check=True)
    subprocess.run(["/usr/bin/git", "commit", "-qm", "initial"], cwd=tmp_path, check=True)
    expected_head = subprocess.run(
        ["/usr/bin/git", "rev-parse", "HEAD"], cwd=tmp_path, check=True,
        capture_output=True, text=True,
    ).stdout.strip()
    subprocess.run(
        ["/usr/bin/git", "update-ref", "refs/remotes/origin/main", expected_head],
        cwd=tmp_path, check=True,
    )
    dot = tmp_path / "provenance.dot"
    dot.write_text(
        "digraph provenance { start [shape=Mdiamond] observe [type=observe] "
        "exit [shape=Msquare] start -> observe -> exit }",
        encoding="utf-8",
    )
    observed = {}

    def observe(node, ctx):
        observed.update(ctx.state)
        return Result(outcome="success")

    monkeypatch.setitem(TYPE_REGISTRY, "observe", observe)
    import runner.handler_audit as handler_audit
    monkeypatch.setattr(handler_audit, "_controller_trust_head", lambda workdir: expected_head)
    run(parse(dot), Context(goal="provenance", workdir=tmp_path, backend="echo"))

    assert observed["_df_controller_trust_head"] == expected_head
    assert "_df_run_initial_workspace_sha256" not in observed


def test_resume_preserves_original_controller_trust_after_worker_commit(
    tmp_path, monkeypatch
):
    subprocess.run(["/usr/bin/git", "init", "-q"], cwd=tmp_path, check=True)
    subprocess.run(["/usr/bin/git", "config", "user.email", "jleechan2015@users.noreply.github.com"], cwd=tmp_path, check=True)
    subprocess.run(["/usr/bin/git", "config", "user.name", "Test"], cwd=tmp_path, check=True)
    (tmp_path / "tracked.txt").write_text("trusted\n")
    subprocess.run(["/usr/bin/git", "add", "."], cwd=tmp_path, check=True)
    subprocess.run(["/usr/bin/git", "commit", "-qm", "trusted"], cwd=tmp_path, check=True)
    trusted = subprocess.run(["/usr/bin/git", "rev-parse", "HEAD"], cwd=tmp_path, check=True, capture_output=True, text=True).stdout.strip()
    subprocess.run(["/usr/bin/git", "update-ref", "refs/remotes/origin/main", trusted], cwd=tmp_path, check=True)
    (tmp_path / "tracked.txt").write_text("worker\n")
    subprocess.run(["/usr/bin/git", "commit", "-qam", "worker"], cwd=tmp_path, check=True)
    worker_head = subprocess.run(["/usr/bin/git", "rev-parse", "HEAD"], cwd=tmp_path, check=True, capture_output=True, text=True).stdout.strip()
    dot = tmp_path / "resume_trust.dot"
    dot.write_text(
        "digraph resume_trust { start [shape=Mdiamond] worker [type=codergen] "
        "observe [type=observe] operator [type=operator_verify] exit [shape=Msquare] "
        "start -> worker -> observe -> operator -> exit }",
        encoding="utf-8",
    )
    checkpoint = tmp_path / "checkpoint.json"
    checkpoint.write_text(json.dumps([{
        "node": "worker", "outcome": "success", "ts": 1,
        "output_preview": "done",
        "metadata": {"_df_controller_trust_head": trusted},
    }]))
    observed = {}
    def observe(node, ctx):
        observed.update(ctx.state)
        return Result(outcome="success")
    monkeypatch.setitem(TYPE_REGISTRY, "observe", observe)
    monkeypatch.setitem(TYPE_REGISTRY, "operator_verify", lambda node, ctx: Result(outcome="success"))
    import runner.handler_audit as handler_audit
    monkeypatch.setattr(handler_audit, "_controller_trust_head", lambda workdir: trusted)

    run(parse(dot), Context(goal="resume trust", workdir=tmp_path, backend="codex"), resume=checkpoint)

    assert observed["_df_controller_trust_head"] == trusted
    assert observed["_df_controller_trust_head"] != worker_head

    checkpoint.write_text(json.dumps([{
        "node": "worker", "outcome": "success", "ts": 1,
        "output_preview": "done",
        "metadata": {"_df_controller_trust_head": worker_head},
    }]))
    with pytest.raises(ValueError, match="controller trust"):
        run(
            parse(dot),
            Context(goal="tampered resume trust", workdir=tmp_path, backend="codex"),
            resume=checkpoint,
        )


def test_resume_fails_closed_without_controller_trust_metadata(tmp_path):
    dot = tmp_path / "resume_missing_trust.dot"
    dot.write_text(
        "digraph resume_missing_trust { start [shape=Mdiamond] worker [type=codergen] "
        "operator [type=operator_verify] exit [shape=Msquare] start -> worker -> operator -> exit }",
        encoding="utf-8",
    )
    checkpoint = tmp_path / "checkpoint.json"
    checkpoint.write_text(json.dumps([{
        "node": "worker", "outcome": "success", "ts": 1,
        "output_preview": "done", "metadata": {},
    }]))

    with pytest.raises(ValueError, match="controller trust"):
        run(parse(dot), Context(goal="resume trust", workdir=ROOT, backend="codex"), resume=checkpoint)


@pytest.mark.parametrize(
    ("outcomes", "expected_calls", "expected_outcome"),
    [
        (["failure", "success"], 2, "success"),
        (["error", "success"], 1, "error"),
    ],
)
def test_node_retries_failure_but_never_terminal_error(
    tmp_path, outcomes, expected_calls, expected_outcome
):
    dot = tmp_path / "retry_contract.dot"
    dot.write_text(
        'digraph retry_contract {\n'
        '  start [shape=Mdiamond]\n'
        '  worker [type="codergen", max_retries="2"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> worker -> exit\n'
        '}\n'
    )
    calls: list[str] = []

    def fake_worker(node, ctx):
        outcome = outcomes[len(calls)]
        calls.append(outcome)
        return Result(outcome=outcome, output=outcome)

    monkeypatch = pytest.MonkeyPatch()
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_worker)
    try:
        history = run(
            parse(dot),
            Context(goal="retry contract", workdir=tmp_path, backend="echo"),
        )
    finally:
        monkeypatch.undo()

    assert len(calls) == expected_calls
    worker_records = [record for record in history if record.node == "worker"]
    assert worker_records[-1].outcome == expected_outcome


def test_parser_round_trip():
    g = parse(_pipeline("hello.dot"))
    assert g.name == "hello"
    assert g.goal == "Minimal smoke pipeline — explore, plan, implement, holdout-eval, exit."
    assert "start" in g.nodes
    assert "exit" in g.nodes
    assert "holdout" in g.nodes
    # fix -> holdout edge exists
    assert any(e.src == "fix" and e.dst == "holdout" for e in g.edges)


def test_model_stylesheet_sets_node_backend_attributes(tmp_path):
    style = tmp_path / "minimal.model.css"
    style.write_text('* { backend: "echo" }\n.hot { backend: "mock_llm" }\n')
    dot = tmp_path / "styled.dot"
    dot.write_text(
        'digraph styled {\n'
        '  graph [goal="styled" model_stylesheet="minimal.model.css"]\n'
        '  start [shape=Mdiamond]\n'
        '  plan [type="codergen"]\n'
        '  implement [type="codergen", class="hot"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> plan -> implement -> exit\n'
        '}\n'
    )
    g = parse(dot)
    assert g.nodes["plan"].attrs["backend"] == "echo"
    assert g.nodes["implement"].attrs["backend"] == "mock_llm"


def test_echo_backend_loops_on_failed_holdout(monkeypatch, tmp_path):
    """If holdout always fails, fix loop should be bounded by max_visits."""
    calls = {"holdout": 0}

    def fake_holdout(node, ctx):
        calls["holdout"] += 1
        return Result(outcome="fail", output="fake fail")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=50)

    # Should terminate via fix-node max_visits=3 (4th visit triggers exhausted).
    assert history[-1].outcome == "exhausted"
    assert history[-1].node == "fix"
    assert calls["holdout"] >= 3


def test_echo_backend_green_path(monkeypatch):
    """Holdout succeeds → straight to exit."""
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="fake pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=50)

    nodes = [r.node for r in history]
    assert nodes == [
        "start",
        "explore_in",
        "explore_fanout",
        "explore_concept",
        "explore_auth",
        "explore_reuse",
        "explore_risks",
        "explore_join",
        "explore_stitch",
        "explore_out",
        "plan",
        "implement",
        "holdout",
        "exit",
    ]
    assert history[-1].outcome == "success"


def test_successful_validation_clears_prior_failure(monkeypatch):
    """A fix loop can recover if a later validation node succeeds."""
    calls = {"holdout": 0}

    def fake_holdout(node, ctx):
        calls["holdout"] += 1
        if calls["holdout"] == 1:
            return Result(outcome="fail", output="first fail")
        return Result(outcome="success", output="recovered")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=50)

    assert [r.node for r in history][-3:] == ["fix", "holdout", "exit"]
    assert history[-1].outcome == "success"


def test_default_event_log_path_uses_final_cxdb_run_id(monkeypatch, tmp_path):
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="fake pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    graph = parse(_pipeline("hello.dot"))
    ctx = Context(goal="event path", workdir=ROOT, backend="echo", cxdb_path=tmp_path / "cxdb.sqlite")

    history = run(graph, ctx, max_steps=50)

    assert history[-1].outcome == "success"
    assert ctx.run_id
    assert ctx.event_log_path.name == "events.jsonl"
    assert ctx.event_log_path.parent.name == ctx.run_id
    events = [
        json.loads(line)
        for line in ctx.event_log_path.read_text().splitlines()
        if line.strip()
    ]
    assert events
    assert {event["run_id"] for event in events} == {ctx.run_id}


def test_cli_invocation_green(tmp_path):
    """End-to-end: run the CLI with the real holdout evaluator against the impl tree."""
    import shutil
    shutil.copytree(ROOT / "impl", tmp_path / "impl")
    proc = subprocess.run(
        [
            sys.executable, "-m", "runner",
            "--pipeline", str(_pipeline("hello.dot")),
            "--goal", "smoke",
            "--backend", "echo",
            "--feature", "hello",
            "--workdir", str(tmp_path),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    # The greet.py impl ships with the repo so the holdout should pass.
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert '"final_outcome": "success"' in proc.stdout


def test_cli_feature_flag_overrides_dot_feature(tmp_path):
    proc = subprocess.run(
        [
            sys.executable, "-m", "runner",
            "--pipeline", str(_pipeline("hello.dot")),
            "--goal", "smoke",
            "--backend", "echo",
            "--feature", "does-not-exist",
            "--workdir", str(tmp_path),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert proc.returncode == 1
    assert '"final_outcome": "success"' not in proc.stdout


def test_cli_missing_holdout_feature_fails_before_run(tmp_path):
    dot = tmp_path / "missing_feature.dot"
    dot.write_text(
        'digraph missing_feature {\n'
        '  start [shape=Mdiamond]\n'
        '  holdout [type="holdout_eval", feature="${state.feature}"]\n'
        '  fix [type="codergen", prompt="@prompts/hello/fix.md"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> holdout\n'
        '  holdout -> exit [condition="outcome=success"]\n'
        '  holdout -> fix [condition="outcome!=success"]\n'
        '  fix -> holdout\n'
        '}\n'
    )
    proc = subprocess.run(
        [
            sys.executable, "-m", "runner",
            "--pipeline", str(dot),
            "--goal", "smoke",
            "--backend", "echo",
            "--workdir", str(tmp_path),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )

    assert proc.returncode == 2
    assert "holdout_eval requires feature metadata before execution" in proc.stderr
    assert "missing for node(s): holdout" in proc.stderr
    assert '"node": "fix"' not in proc.stdout
    assert not (tmp_path / "evidence").exists()


def test_max_steps_before_exit_is_failure():
    """A run that stops before reaching exit must not report success."""
    g = parse(_pipeline("hello.dot"))
    ctx = Context(goal="test", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=1)

    assert history[-1].outcome == "exhausted"
    assert "max_steps=1" in history[-1].output_preview


def test_engine_resume_from_checkpoint(tmp_path):
    dot = tmp_path / "resume.dot"
    dot.write_text(
        'digraph resume {\n'
        '  graph [goal="Resume test" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  one [type="codergen", label="One"]\n'
        '  two [type="codergen", label="Two"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> one -> two -> exit\n'
        '}\n'
    )
    checkpoint = tmp_path / "checkpoint.json"
    checkpoint.write_text(
        json.dumps(
            [
                {
                    "node": "start",
                    "outcome": "success",
                    "ts": 0.0,
                    "output_preview": "start",
                    "metadata": {},
                },
                {
                    "node": "one",
                    "outcome": "success",
                    "ts": 0.0,
                    "output_preview": "one",
                    "metadata": {},
                },
            ]
        )
    )
    graph = parse(dot)
    ctx = Context(goal="resume", workdir=ROOT, backend="echo")
    history = run(graph, ctx, resume=checkpoint, max_steps=10)

    assert [step.node for step in history] == ["start", "one", "two", "exit"]


def test_engine_parallel_fanout_with_join_quorum_and_allow_partial(tmp_path, monkeypatch):
    dot = tmp_path / "parallel.dot"
    dot.write_text(
        'digraph parallel {\n'
        '  graph [goal="Parallel gate test" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  fan [type="codergen", label="Fanout", parallel="true", allow_partial="true", join_quorum=2]\n'
        '  branch_pass [type="branch_pass", label="Branch pass"]\n'
        '  branch_partial [type="branch_partial", label="Branch partial"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> fan\n'
        '  fan -> branch_pass\n'
        '  fan -> branch_partial\n'
        '  fan -> exit [join=true]\n'
        '}\n'
    )
    monkeypatch.setitem(TYPE_REGISTRY, "branch_pass", lambda node, ctx: Result(outcome="success", output="ok"))
    monkeypatch.setitem(TYPE_REGISTRY, "branch_partial", lambda node, ctx: Result(outcome="partial", output="ok"))

    graph = parse(dot)
    ctx = Context(goal="parallel", workdir=ROOT, backend="echo")
    history = run(graph, ctx, max_steps=20)

    assert [step.node for step in history] == [
        "start",
        "fan",
        "branch_pass",
        "branch_partial",
        "exit",
    ]
    assert history[-1].outcome == "success"


def test_recursive_boolean_edge_matching():
    from runner.engine import _evaluate_expression
    from runner.handlers import Result, Context

    ctx = Context(goal="test", workdir=None, backend="echo")
    ctx.state["error_code"] = "404"
    ctx.state["test_failures"] = "critical, warning"

    last = Result(outcome="success", metadata={"api_key": "valid"})

    # Simple outcome check
    assert _evaluate_expression("success", last, ctx, False) is True
    assert _evaluate_expression("fail", last, ctx, False) is False

    # EQ and NEQ comparisons
    assert _evaluate_expression("outcome = success", last, ctx, False) is True
    assert _evaluate_expression("outcome != success", last, ctx, False) is False

    # Metadata lookup
    assert _evaluate_expression("api_key == valid", last, ctx, False) is True
    assert _evaluate_expression("api_key != invalid", last, ctx, False) is True

    # State lookup
    assert _evaluate_expression("error_code = 404", last, ctx, True) is True
    assert _evaluate_expression("error_code != 500", last, ctx, True) is True

    # CONTAINS and NOT CONTAINS
    assert _evaluate_expression("test_failures contains critical", last, ctx, True) is True
    assert _evaluate_expression("test_failures not contains blocker", last, ctx, True) is True

    # IN and NOT IN
    assert _evaluate_expression("error_code in '404, 500'", last, ctx, True) is True
    assert _evaluate_expression("error_code not in '200, 301'", last, ctx, True) is True

    # Compound boolean expressions (AND, OR, NOT)
    assert _evaluate_expression("outcome = success && error_code = 404", last, ctx, True) is True
    assert _evaluate_expression("outcome = success && error_code = 500", last, ctx, True) is False
    assert _evaluate_expression("outcome = fail || error_code = 404", last, ctx, True) is True
    assert _evaluate_expression("!(error_code = 500)", last, ctx, True) is True

    # Nested parenthesized expressions
    assert _evaluate_expression("outcome = success && (error_code = 500 || test_failures contains critical)", last, ctx, True) is True
    assert _evaluate_expression("outcome = success && !(error_code = 500 || test_failures contains blocker)", last, ctx, True) is True


def test_structured_events_and_transcripts(monkeypatch, tmp_path):
    import hashlib
    # We will write a custom dot file with a node that has max_retries set,
    # and a mock implementation that fails first and then succeeds, or fails always.
    dot_file = tmp_path / "test_retries.dot"
    dot_file.write_text(
        'digraph test_retries {\n'
        '  graph [goal="testing events"]\n'
        '  start [shape=Mdiamond]\n'
        '  node_a [type="codergen", max_retries="2"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> node_a -> exit\n'
        '}\n'
    )
    
    attempts = []
    def fake_codergen(node, ctx):
        attempts.append(len(attempts) + 1)
        if len(attempts) < 3:
            return Result(outcome="fail", output=f"fail attempt {len(attempts)}")
        return Result(outcome="success", output="pass attempt 3")
        
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_codergen)
    
    g = parse(dot_file)
    ctx = Context(goal="test retries", workdir=ROOT, backend="echo")
    checkpoint_file = tmp_path / "checkpoint.json"
    
    history = run(g, ctx, checkpoint=checkpoint_file, max_steps=50)
    
    # Verify execution outcome
    assert len(attempts) == 3
    assert history[-1].outcome == "success"
    
    # Read the event log
    assert ctx.event_log_path is not None
    assert ctx.event_log_path.exists()
    
    events = [
        json.loads(line)
        for line in ctx.event_log_path.read_text().splitlines()
        if line.strip()
    ]
    
    node_starts = [e for e in events if e["event"] == "node_start" and e.get("node") == "node_a"]
    assert len(node_starts) == 3
    assert node_starts[0]["attempt"] == "1"
    assert node_starts[1]["attempt"] == "2"
    assert node_starts[2]["attempt"] == "3"
    
    retries = [e for e in events if e["event"] == "retry" and e.get("node") == "node_a"]
    assert len(retries) == 2
    assert retries[0]["attempt"] == "2"
    assert retries[1]["attempt"] == "3"
    
    node_results = [e for e in events if e["event"] == "node_result" and e.get("node") == "node_a"]
    assert len(node_results) == 3
    assert node_results[0]["attempt"] == "1"
    assert node_results[0]["outcome"] == "failure"
    assert node_results[1]["attempt"] == "2"
    assert node_results[1]["outcome"] == "failure"
    assert node_results[2]["attempt"] == "3"
    assert node_results[2]["outcome"] == "success"
    
    # Check that transcript sidecars were written and contain correct hashes and content
    for r in node_results:
        assert "input_path" in r
        assert "input_sha256" in r
        input_path = pathlib.Path(r["input_path"])
        assert input_path.exists()
        input_content = input_path.read_text()
        assert "Dark Factory node input" in input_content
        assert '"node": "node_a"' in input_content
        assert r["input_sha256"] == hashlib.sha256(input_content.encode("utf-8")).hexdigest()

        assert "transcript_path" in r
        assert "transcript_sha256" in r
        path = pathlib.Path(r["transcript_path"])
        assert path.exists()
        content = path.read_text()
        assert r["transcript_sha256"] == hashlib.sha256(content.encode("utf-8")).hexdigest()
        if r["attempt"] == "1":
            assert content == "fail attempt 1"
        elif r["attempt"] == "2":
            assert content == "fail attempt 2"
        elif r["attempt"] == "3":
            assert content == "pass attempt 3"
            
    # Check checkpoint events
    checkpoint_events = [e for e in events if e["event"] == "checkpoint"]
    assert len(checkpoint_events) > 0
    assert all("path" in e for e in checkpoint_events)

    # Let's also verify holdout transcript redaction
    # We will write a custom dot file with a holdout node
    dot_file_holdout = tmp_path / "test_holdout_redaction.dot"
    dot_file_holdout.write_text(
        'digraph test_holdout {\n'
        '  graph [goal="testing holdout"]\n'
        '  start [shape=Mdiamond]\n'
        '  holdout [type="holdout_eval"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> holdout -> exit\n'
        '}\n'
    )
    
    def fake_holdout_node(node, ctx):
        return Result(outcome="success", output="super secret holdout details")
        
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout_node)
    
    g_holdout = parse(dot_file_holdout)
    ctx_holdout = Context(goal="test holdout", workdir=ROOT, backend="echo")
    history_holdout = run(g_holdout, ctx_holdout, max_steps=50)
    
    assert history_holdout[-1].outcome == "success"
    events_holdout = [
        json.loads(line)
        for line in ctx_holdout.event_log_path.read_text().splitlines()
        if line.strip()
    ]
    
    holdout_result = next(e for e in events_holdout if e["event"] == "node_result" and e.get("node") == "holdout")
    assert "input_path" in holdout_result
    assert pathlib.Path(holdout_result["input_path"]).exists()
    assert "transcript_path" in holdout_result
    holdout_path = pathlib.Path(holdout_result["transcript_path"])
    assert holdout_path.exists()
    assert holdout_path.read_text() == "<redacted holdout output>"
