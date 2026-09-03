"""Tests for the fix-loop test-awareness fixes (D1+D2+D3+D4, 2026-06-22).

Regression coverage for the dark-factory failure on run 7aa7695b1cf6
(PR-B'' read-shim), where three independent defects compounded:
  - D1: stale --state slim.test_command referenced missing .py files,
        pytest `file not found` rc=4 was accepted as a real test result
  - D2: prompts/slim/fix.md had no ${state.last_test_output} substitution,
        so the fix agent was blind to the actual failure
  - D3: max_visits=3 bound on visits, not on outcome, allowed 4 blind
        fix iterations to burn ~25 minutes of LLM time

These tests pin the new behavior so the same failure mode cannot recur
without surfacing in CI.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline  # noqa: E402

from runner.engine import run  # noqa: E402
from runner.handler_control import (  # noqa: E402
    _check_test_command_paths,
    _coerce_goal_gate,
    _extract_pytest_paths,
    _is_pytest_command,
    _record_test_failure_state,
    _tool,
)
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.parser import Node, parse  # noqa: E402

# -- D1 ------------------------------------------------------------------


def test_is_pytest_command_detects_python_m_pytest():
    assert _is_pytest_command("python3 -m pytest tests/test_x.py")
    assert _is_pytest_command("python -m pytest tests/")
    assert _is_pytest_command("/usr/bin/pytest tests/test_x.py")
    assert _is_pytest_command("PYTHONPATH=. python -m pytest")
    # Env-var form (PEP 263): PYTEST=1 ... is parsed by shlex as a
    # separate VAR=VALUE token, so it does not pollute the test.
    assert _is_pytest_command("PYTEST=1 python -m pytest tests/")
    # Absolute path with versioned suffix
    assert _is_pytest_command("/usr/local/bin/pytest-3 tests/test_x.py")
    # Non-pytest commands
    assert not _is_pytest_command("git grep foo")
    assert not _is_pytest_command("cat spec.md")
    assert not _is_pytest_command("ls -la")


def test_extract_pytest_paths_skips_flags():
    cmd = "python3 -m pytest tests/test_a.py -v tests/test_b.py --tb=short tests/test_c.py"
    paths = _extract_pytest_paths(cmd)
    assert paths == [
        "tests/test_a.py",
        "tests/test_b.py",
        "tests/test_c.py",
    ]


def test_check_test_command_paths_returns_none_when_all_exist(tmp_path):
    (tmp_path / "a.py").write_text("")
    (tmp_path / "b.py").write_text("")
    cmd = f"python3 -m pytest {tmp_path}/a.py {tmp_path}/b.py -v"
    assert _check_test_command_paths(cmd, tmp_path) is None


def test_check_test_command_paths_flags_missing_file(tmp_path):
    (tmp_path / "a.py").write_text("")
    # b.py intentionally not created
    cmd = f"python3 -m pytest {tmp_path}/a.py {tmp_path}/b.py -v"
    err = _check_test_command_paths(cmd, tmp_path)
    assert err is not None
    assert "b.py" in err
    assert "missing file" in err.lower()
    assert "test_command" in err


def test_check_test_command_paths_skips_non_pytest(tmp_path):
    """Non-pytest commands must NOT trigger the path check, even if they
    contain ghost .py tokens in args.
    """
    cmd = f"cat {tmp_path}/does_not_exist.py"
    assert _check_test_command_paths(cmd, tmp_path) is None


def test_tool_returns_failure_for_missing_test_file(tmp_path, monkeypatch):
    """End-to-end: a goal_gate tool node with a pytest command referencing
    a missing .py must fail with a clear 'missing file' message — not
    silently run pytest and return its rc=4.
    """
    # Bypass sandbox-exec: not all CI environments have it wired.
    from runner import handlers as _handlers_shim

    monkeypatch.setattr(
        _handlers_shim, "_sandboxed_args",
        lambda args: list(args),
    )
    monkeypatch.setattr(
        _handlers_shim, "_sanitized_env", lambda: {},
    )

    present = tmp_path / "present.py"
    present.write_text("")
    missing = tmp_path / "missing.py"
    cmd = f"python3 -m pytest {present} {missing} -v"

    node = Node(
        name="test",
        attrs={"command": cmd, "goal_gate": "true", "timeout": "60"},
    )
    ctx = Context(goal="t", workdir=tmp_path, backend="echo")
    result = _tool(node, ctx)

    assert result.outcome == "failure"
    assert "missing" in result.output.lower()
    assert "missing.py" in result.output
    assert result.metadata.get("missing_test_files") == "true"
    assert result.metadata.get("command") == cmd
    # And it must NOT have populated the last_test_output state key —
    # we only want to record real test runs, not pre-flight failures.
    assert "last_test_output" not in ctx.state


# -- D2 ------------------------------------------------------------------


def test_record_test_failure_state_writes_three_keys():
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    _record_test_failure_state(
        ctx,
        cmd="python3 -m pytest foo.py",
        rc="4",
        output="===== ERRORS =====\nfixture 'bar' not found\n",
    )
    assert ctx.state["last_test_command"] == "python3 -m pytest foo.py"
    assert ctx.state["last_test_rc"] == "4"
    assert "fixture 'bar' not found" in ctx.state["last_test_output"]


def test_record_test_failure_state_truncates_huge_output():
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    huge = "x" * 20_000
    _record_test_failure_state(ctx, cmd="pytest", rc="1", output=huge)
    # 4000 chars + truncation marker
    assert len(ctx.state["last_test_output"]) <= 4100
    assert "[truncated]" in ctx.state["last_test_output"]


def test_record_test_failure_state_is_first_wins():
    """A later failure must NOT overwrite the canonical first failure —
    the fix prompt benefits from a stable, citable record of what went
    wrong the first time.
    """
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    _record_test_failure_state(ctx, cmd="pytest a.py", rc="1", output="first failure")
    _record_test_failure_state(ctx, cmd="pytest b.py", rc="2", output="second failure")
    assert "first failure" in ctx.state["last_test_output"]
    assert "second failure" not in ctx.state["last_test_output"]


def test_tool_records_test_failure_state_on_real_run(tmp_path, monkeypatch):
    """End-to-end: a goal_gate tool node that runs a command and gets
    rc=1 (e.g. a real pytest failure, not the file-not-found case which
    D1 catches earlier) must record the failure state for the fix
    prompt to consume. We use `sh -c "exit 1"` to simulate a real
    failure without setting up a real pytest fixture.
    """
    from runner import handlers as _handlers_shim

    monkeypatch.setattr(
        _handlers_shim, "_sandboxed_args",
        lambda args: list(args),
    )
    monkeypatch.setattr(
        _handlers_shim, "_sanitized_env", lambda: {},
    )

    # `false` exits 1 cleanly with no output; it is NOT a pytest
    # command, so D1's pre-flight does not fire.
    cmd = "false"

    node = Node(
        name="test",
        attrs={"command": cmd, "goal_gate": "true", "timeout": "30"},
    )
    ctx = Context(goal="t", workdir=tmp_path, backend="echo")
    result = _tool(node, ctx)

    # The D2 path: outcome=failure (rc=1) and goal_gate=true, so the
    # last_test_* state keys must be populated for the fix prompt.
    assert result.outcome == "failure"
    assert ctx.state["last_test_command"] == cmd
    assert ctx.state["last_test_rc"] == "1"
    assert "last_test_output" in ctx.state


def test_tool_does_not_record_state_on_success():
    """A goal_gate tool node that passes must NOT pollute last_test_output
    (which is reserved for the failure that the fix prompt needs to see).
    """
    from runner import handlers as _handlers_shim
    import runner.handler_control as _hc

    # Force a real success by patching _check_test_command_paths + the run.
    monkey = __import__("pytest").MonkeyPatch()
    try:
        monkey.setattr(_hc, "_check_test_command_paths", lambda cmd, cwd: None)
        monkey.setattr(
            _handlers_shim, "_sandboxed_args",
            lambda args: list(args),
        )
        monkey.setattr(
            _handlers_shim, "_sanitized_env", lambda: {},
        )
        # Use 'true' (exits 0) as the command — bypasses pytest path check.
        node = Node(
            name="test",
            attrs={"command": "true", "goal_gate": "true", "timeout": "30"},
        )
        ctx = Context(goal="t", workdir=ROOT, backend="echo")
        result = _tool(node, ctx)
        assert result.outcome == "success"
        assert "last_test_output" not in ctx.state
    finally:
        monkey.undo()


def test_tool_executes_compound_shell_commands(monkeypatch, tmp_path):
    """A tool node with compound shell operators (&&, ||, ;, |) must execute via bash -c."""
    from runner import handlers as _handlers_shim
    import runner.handler_control as _hc

    monkeypatch.setattr(_hc, "_check_test_command_paths", lambda cmd, cwd: None)
    monkeypatch.setattr(_handlers_shim, "_sandboxed_args", lambda args: list(args))
    monkeypatch.setattr(_handlers_shim, "_sanitized_env", lambda: {})

    node = Node(
        name="test",
        attrs={"command": "echo first && echo second", "goal_gate": "true", "timeout": "30"},
    )
    ctx = Context(goal="t", workdir=tmp_path, backend="echo")
    result = _tool(node, ctx)
    assert result.outcome == "success"
    assert "first" in result.output
    assert "second" in result.output


def test_coerce_goal_gate_accepts_string_and_bool():
    assert _coerce_goal_gate(Node(name="n", attrs={"goal_gate": True})) is True
    assert _coerce_goal_gate(Node(name="n", attrs={"goal_gate": "true"})) is True
    assert _coerce_goal_gate(Node(name="n", attrs={"goal_gate": "1"})) is True
    assert _coerce_goal_gate(Node(name="n", attrs={"goal_gate": "yes"})) is True
    assert _coerce_goal_gate(Node(name="n", attrs={"goal_gate": "false"})) is False
    assert _coerce_goal_gate(Node(name="n", attrs={})) is False


# -- D3 ------------------------------------------------------------------


def test_no_progress_short_circuits_fix_loop(monkeypatch, tmp_path):
    """Reproduces the 7aa7695b1cf6 failure pattern: a fix node returns
    identical 'success' output 2+ times in a row. With no_progress_max="2",
    the engine short-circuits to 'exhausted' before the 3rd blind call.
    """
    # Build a tiny minimal pipeline with no_progress_max="2" on the fix
    # node. This isolates the D3 behavior without depending on the
    # main repo's existing .dot files (which would couple this test to
    # future changes in hello.dot / minimal_pr.dot topology).
    pipeline_dot = """\
digraph NoProgressTest {
  graph [goal="test no_progress_max short-circuit"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare,  label="Exit"]

  test [type="tool", label="Always Fail Test",
        command="false", goal_gate="true", retry_target="fix"]

  fix [type="codergen", label="Blind Fix",
       prompt="@prompts/slim/fix.md",
       max_visits="5", no_progress_max="2"]

  start -> test
  test -> fix [condition="outcome!=success"]
  test -> exit [condition="outcome=success"]
  fix -> test
}
"""
    pipeline_path = tmp_path / "no_progress.dot"
    pipeline_path.write_text(pipeline_dot)

    fix_calls = {"n": 0}

    def fake_tool(node, ctx):
        # Pretend the test command failed with a generic error so the
        # test node's "command" state is set up the way a real run would
        # leave it. We use the "false" command for simplicity.
        return Result(outcome="failure", output="forced fail", metadata={})

    def fake_fix(node, ctx):
        fix_calls["n"] += 1
        # Always return the same output — this is the "blind fix" case.
        return Result(
            outcome="success",
            output="same output every time",
            metadata={"attempt": str(fix_calls["n"])},
        )

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_tool)
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_fix)

    g = parse(pipeline_path, require_start_exit=True)
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=50)

    # The fix node should be visited at most 2 times before the no_progress
    # short-circuit fires. (The 1st visit's hash is added, the 2nd matches,
    # no_progress_max=2 reached → exhausted.)
    assert fix_calls["n"] <= 2, (
        f"no_progress_max=2 should have stopped the fix loop, "
        f"but fix was called {fix_calls['n']} times"
    )
    assert history[-1].outcome == "exhausted"


def test_no_progress_allows_progress_then_blocks(monkeypatch, tmp_path):
    """Inverse: if the fix node produces DIFFERENT output across visits,
    no_progress_max must NOT short-circuit — the loop should continue.
    """
    # Same in-test pipeline as test_no_progress_short_circuits_fix_loop,
    # but with max_visits bumped higher so we can verify progress-based
    # continuation. We assert only that fix is called >= 2 times
    # (diverse outputs, no short-circuit).
    pipeline_dot = """\
digraph NoProgressTest2 {
  graph [goal="test no_progress_max allows diverse outputs"]
  start [shape=Mdiamond, label="Start"]
  exit  [shape=Msquare,  label="Exit"]

  test [type="tool", label="Always Fail Test",
        command="false", goal_gate="true", retry_target="fix"]

  fix [type="codergen", label="Diverse Fix",
       prompt="@prompts/slim/fix.md",
       max_visits="5", no_progress_max="2"]

  start -> test
  test -> fix [condition="outcome!=success"]
  test -> exit [condition="outcome=success"]
  fix -> test
}
"""
    pipeline_path = tmp_path / "no_progress2.dot"
    pipeline_path.write_text(pipeline_dot)

    fix_calls = {"n": 0}

    def fake_tool(node, ctx):
        return Result(outcome="failure", output="forced fail", metadata={})

    def fake_fix(node, ctx):
        fix_calls["n"] += 1
        return Result(
            outcome="success",
            output=f"different output #{fix_calls['n']}",
        )

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_tool)
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", fake_fix)

    g = parse(pipeline_path, require_start_exit=True)
    ctx = Context(goal="t", workdir=ROOT, backend="echo")
    history = run(g, ctx, max_steps=200)

    # The fix node should be visited at least 2 times (diverse outputs).
    # We don't assert an exact count because the engine may also consult
    # max_visits=5; the invariant is that no_progress does NOT fire
    # when outputs differ.
    assert fix_calls["n"] >= 2


# -- D4 ------------------------------------------------------------------


def test_fix_md_prompt_substitutes_failure_handoff_state(monkeypatch, tmp_path):
    """End-to-end: prompts/slim/fix.md must consume failure handoff state.
    When the runner renders the fix prompt with last_test_* keys in
    ctx.state and the previous node's free-form output in _last_output,
    the placeholders must be replaced.
    """
    from runner.handler_render import _render_prompt
    from runner.handlers import Context

    # Use ROOT (the dark-factory repo) as workdir so that
    # ROOT/prompts/slim/fix.md resolves correctly. _render_prompt falls
    # back to factory_home() if workdir/<ref> doesn't exist, so this
    # works either way.
    # The Node stores the prompt path in attrs["prompt"] (with leading
    # `@`); the `prompt_ref` property strips it.
    node = Node(name="fix", attrs={"prompt": "@prompts/slim/fix.md"})
    ctx = Context(goal="fix the failing test", workdir=ROOT, backend="echo")
    ctx.state["last_test_command"] = "python3 -m pytest tests/test_x.py"
    ctx.state["last_test_rc"] = "4"
    ctx.state["last_test_output"] = "fixture 'bar' not found"
    ctx.state["_last_output"] = (
        "Evidence reviewer finding: missing SHA-bound browser video proof."
    )

    rendered = _render_prompt(node, ctx)

    # No literal placeholder should survive the substitution.
    assert "${state._last_output}" not in rendered
    assert "${state.last_test_command}" not in rendered
    assert "${state.last_test_rc}" not in rendered
    assert "${state.last_test_output}" not in rendered
    # And the values must appear in the rendered prompt.
    assert "missing SHA-bound browser video proof" in rendered
    assert "python3 -m pytest tests/test_x.py" in rendered
    assert "fixture 'bar' not found" in rendered
    # Returncode 4 is recorded; verify it appears in the fix prompt.
    assert "4" in rendered
