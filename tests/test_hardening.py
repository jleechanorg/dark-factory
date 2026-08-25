"""Regression tests for iteration-2 hardening fixes.

Covers:
  - _parse_verdict marker discipline + standalone fallback
  - CXDB WAL pragma + concurrent-writer survival
  - engine._attr_int defensive default on bad attrs
  - engine.run records runs.ended_ts via try/finally even on stuck pipelines
"""

from __future__ import annotations

import json
import os
import pathlib
import sqlite3
import sys
import tempfile
import time
from types import SimpleNamespace

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

# Scratch workdir in the OS tempdir — using the repo root here leaks one
# branch_* mkdtemp per fan-out test into the working tree.
SCRATCH = pathlib.Path(tempfile.mkdtemp(prefix="test_hardening_"))

from conftest import _pipeline, register_scratch_dir  # noqa: E402

register_scratch_dir(SCRATCH)

import runner.handlers as handlers_mod  # noqa: E402
import runner.handler_sandbox as sandbox_mod  # noqa: E402
from runner.cxdb import CXDB  # noqa: E402
from runner.engine import _attr_int, _edge_matches, run  # noqa: E402
from runner.handlers import (  # noqa: E402
    Context,
    Result,
    TYPE_REGISTRY,
    _codergen,
    _holdout_eval,
    _holdouts_repo_path,
    _minimax_env,
    _parse_verdict,
    _render_prompt,
    _sanitized_env,
    _scoped_claude_env,
)
from runner.parser import Edge, parse  # noqa: E402
from runner.parser import Node, parse  # noqa: E402


# Resolved once; the hardcoded /Users/jleechan/... was a CLAUDE.md
# discipline violation (path unreachable on dev machines without the
# sealed repo; non-portable).
SEALED_HOLDOUTS_REPO = pathlib.Path.home() / "projects" / "dark-factory-holdouts"


def test_scoped_claude_env_scrubs_all_provider_overrides(monkeypatch, tmp_path):
    config = tmp_path / "project-claude"
    config.mkdir()
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(config))
    for key in (
        "CLAUDE_CONFIG_DIR",
        "MINIMAX_API_KEY",
        "MINIMAX_BASE_URL",
        "MINIMAX_MODEL",
        "DARK_FACTORY_MINIMAX_MODEL",
        "CLAUDEM_MODE",
        "CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_SKIP_VERTEX_AUTH",
        "AWS_ACCESS_KEY_ID",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_BEDROCK_MODEL_ID",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_GENAI_USE_VERTEXAI",
        "CLOUD_ML_REGION",
        "AZURE_OPENAI_ENDPOINT",
        "OPENAI_API_KEY",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
    ):
        monkeypatch.setenv(key, "stale")

    env = _scoped_claude_env()

    assert env["CLAUDE_CONFIG_DIR"] == str(config.resolve())
    assert not any(key.startswith("ANTHROPIC_") for key in env)
    for key in (
        "MINIMAX_API_KEY",
        "MINIMAX_BASE_URL",
        "MINIMAX_MODEL",
        "DARK_FACTORY_MINIMAX_MODEL",
        "CLAUDEM_MODE",
        "CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_SKIP_VERTEX_AUTH",
        "AWS_ACCESS_KEY_ID",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_BEDROCK_MODEL_ID",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_GENAI_USE_VERTEXAI",
        "CLOUD_ML_REGION",
        "AZURE_OPENAI_ENDPOINT",
        "OPENAI_API_KEY",
    ):
        assert key not in env


def test_scoped_claude_env_rejects_symlink_to_personal_config(monkeypatch, tmp_path):
    login_home = tmp_path / "login-home"
    personal = login_home / ".claude"
    personal.mkdir(parents=True)
    link = tmp_path / "project-config"
    link.symlink_to(personal, target_is_directory=True)
    monkeypatch.setattr(
        sandbox_mod.pwd,
        "getpwuid",
        lambda _uid: SimpleNamespace(pw_dir=str(login_home)),
    )
    monkeypatch.setenv("HOME", str(tmp_path / "fake-home"))
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(link))

    with pytest.raises(ValueError, match="personal ~/.claude"):
        _scoped_claude_env()


def test_scoped_claude_env_rejects_login_users_personal_config_when_home_is_mutated(
    monkeypatch, tmp_path
):
    """A child-controlled HOME must not redefine the login user's ~/.claude."""
    login_home = tmp_path / "login-home"
    personal = login_home / ".claude"
    personal.mkdir(parents=True)
    fake_home = tmp_path / "fake-home"
    fake_home.mkdir()
    monkeypatch.setenv("HOME", str(fake_home))
    monkeypatch.setattr(
        sandbox_mod.pwd,
        "getpwuid",
        lambda _uid: SimpleNamespace(pw_dir=str(login_home)),
    )
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(personal))

    with pytest.raises(ValueError, match="personal ~/.claude"):
        _scoped_claude_env()


def test_scoped_claude_env_fails_closed_when_login_identity_cannot_resolve(
    monkeypatch, tmp_path
):
    config = tmp_path / "project-config"
    config.mkdir()
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(config))

    def missing_uid(_uid):
        raise KeyError("missing uid")

    monkeypatch.setattr(sandbox_mod.pwd, "getpwuid", missing_uid)

    with pytest.raises(ValueError, match="login user's home"):
        _scoped_claude_env()


@pytest.mark.parametrize("critical_name", [".credentials.json", "settings.json"])
def test_scoped_claude_env_rejects_critical_symlink_into_personal_tree(
    monkeypatch, tmp_path, critical_name
):
    login_home = tmp_path / "login-home"
    personal = login_home / ".claude"
    personal.mkdir(parents=True)
    config = tmp_path / "project-config"
    config.mkdir()
    (personal / critical_name).write_text("personal\n")
    (config / critical_name).symlink_to(personal / critical_name)
    monkeypatch.setattr(
        sandbox_mod.pwd,
        "getpwuid",
        lambda _uid: SimpleNamespace(pw_dir=str(login_home)),
    )
    monkeypatch.setenv("HOME", str(tmp_path / "fake-home"))
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(config))

    with pytest.raises(ValueError, match="critical file"):
        _scoped_claude_env()


@pytest.mark.parametrize("critical_name", [".credentials.json", ".claude.json"])
@pytest.mark.parametrize("copy_kind", ["copy", "hardlink"])
def test_scoped_claude_env_rejects_copied_or_hardlinked_personal_credentials(
    monkeypatch, tmp_path, critical_name, copy_kind
):
    login_home = tmp_path / "login-home"
    personal = login_home / ".claude"
    personal.mkdir(parents=True)
    config = tmp_path / "project-config"
    config.mkdir()
    source = personal / critical_name
    source.write_text('{"account":"personal"}\n')
    target = config / critical_name
    if copy_kind == "hardlink":
        os.link(source, target)
    else:
        target.write_bytes(source.read_bytes())
    monkeypatch.setattr(
        sandbox_mod.pwd,
        "getpwuid",
        lambda _uid: SimpleNamespace(pw_dir=str(login_home)),
    )
    monkeypatch.setenv("HOME", str(tmp_path / "fake-home"))
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(config))

    with pytest.raises(ValueError, match="personal credential"):
        _scoped_claude_env()


@pytest.mark.parametrize("critical_name", ["settings.json", "mcp-strict.json"])
def test_scoped_claude_env_does_not_compare_benign_profile_files(
    monkeypatch, tmp_path, critical_name
):
    login_home = tmp_path / "login-home"
    personal = login_home / ".claude"
    personal.mkdir(parents=True)
    config = tmp_path / "project-config"
    config.mkdir()
    source = personal / critical_name
    source.write_text("shared-benign-profile\n")
    (config / critical_name).write_bytes(source.read_bytes())
    monkeypatch.setattr(
        sandbox_mod.pwd,
        "getpwuid",
        lambda _uid: SimpleNamespace(pw_dir=str(login_home)),
    )
    monkeypatch.setenv("HOME", str(tmp_path / "fake-home"))
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(config))

    assert _scoped_claude_env()["CLAUDE_CONFIG_DIR"] == str(config.resolve())


def test_scoped_claude_env_accepts_regular_independent_critical_files(monkeypatch, tmp_path):
    login_home = tmp_path / "login-home"
    (login_home / ".claude").mkdir(parents=True)
    config = tmp_path / "project-config"
    config.mkdir()
    for name in (".credentials.json", ".claude.json", "settings.json", "mcp-strict.json"):
        (config / name).write_text("independent\n")
    monkeypatch.setattr(
        sandbox_mod.pwd,
        "getpwuid",
        lambda _uid: SimpleNamespace(pw_dir=str(login_home)),
    )
    monkeypatch.setenv("HOME", str(tmp_path / "fake-home"))
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(config))

    assert _scoped_claude_env()["CLAUDE_CONFIG_DIR"] == str(config.resolve())


@pytest.mark.parametrize("value", [None, "", "   "])
def test_minimax_env_requires_nonempty_key(monkeypatch, value):
    if value is None:
        monkeypatch.delenv("MINIMAX_API_KEY", raising=False)
    else:
        monkeypatch.setenv("MINIMAX_API_KEY", value)
    with pytest.raises(ValueError, match="MINIMAX_API_KEY"):
        _minimax_env()


def test_minimax_env_scrubs_provider_state_and_uses_model_fallback(monkeypatch):
    monkeypatch.setenv("MINIMAX_API_KEY", "key")
    monkeypatch.setenv("DARK_FACTORY_MINIMAX_MODEL", "   ")
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", "/home/operator/.claude")
    monkeypatch.setenv("ANTHROPIC_AUTH_TOKEN", "stale")
    monkeypatch.setenv("MINIMAX_BASE_URL", "https://stale.minimax.example")
    monkeypatch.setenv("MINIMAX_MODEL", "stale-minimax-model")
    for key in (
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_SKIP_VERTEX_AUTH",
        "AWS_ACCESS_KEY_ID",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_BEDROCK_MODEL_ID",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_GENAI_USE_VERTEXAI",
        "CLOUD_ML_REGION",
        "AZURE_OPENAI_ENDPOINT",
        "OPENAI_API_KEY",
    ):
        monkeypatch.setenv(key, "stale")

    env = _minimax_env()

    assert env["ANTHROPIC_API_KEY"] == "key"
    assert env["ANTHROPIC_BASE_URL"] == "https://api.minimax.io/anthropic"
    assert env["ANTHROPIC_MODEL"] == "MiniMax-M3"
    assert env["ANTHROPIC_SMALL_FAST_MODEL"] == "MiniMax-M3"
    assert "CLAUDE_CONFIG_DIR" not in env
    assert "ANTHROPIC_AUTH_TOKEN" not in env
    for key in (
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_SKIP_VERTEX_AUTH",
        "AWS_ACCESS_KEY_ID",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_BEDROCK_MODEL_ID",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_GENAI_USE_VERTEXAI",
        "CLOUD_ML_REGION",
        "AZURE_OPENAI_ENDPOINT",
        "OPENAI_API_KEY",
    ):
        assert key not in env
    assert "MINIMAX_BASE_URL" not in env
    assert "MINIMAX_MODEL" not in env
    assert "DARK_FACTORY_MINIMAX_MODEL" not in env


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
    edge = Edge(src="a", dst="b", attrs={"condition": "outcome=success@"})
    assert _edge_matches(edge, Result(outcome="success")) is False


def test_parser_rejects_condition_with_unmatched_characters(tmp_path):
    dot = tmp_path / "bad-condition.dot"
    dot.write_text(
        'digraph bad_condition {\n'
        '  start [shape=Mdiamond]\n'
        '  work [type="tool", command="echo ok"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> work\n'
        '  work -> exit [condition="outcome=success@"]\n'
        '}\n'
    )

    with pytest.raises(ValueError, match="malformed condition"):
        parse(dot)


def test_bare_word_factor_in_compound_expression_checks_outcome():
    """A bare word as the first factor in a compound expression (e.g. 'success && k=v')
    must compare against outcome, not look the word up as a state key."""
    edge_match = Edge(src="a", dst="b", attrs={"condition": "success && retry_count=0"})
    edge_fail = Edge(src="a", dst="b", attrs={"condition": "success && retry_count=0"})
    result_success = Result(outcome="success", metadata={"retry_count": "0"})
    result_fail = Result(outcome="failure", metadata={"retry_count": "0"})
    assert _edge_matches(edge_match, result_success) is True
    assert _edge_matches(edge_fail, result_fail) is False


def test_malformed_hyphenated_edge_conditions_fail_closed():
    edge_in = Edge(src="a", dst="b", attrs={"condition": "not-in-list"})
    edge_contains = Edge(src="a", dst="b", attrs={"condition": "not-contains-x"})
    assert _edge_matches(edge_in, Result(outcome="success")) is False
    assert _edge_matches(edge_contains, Result(outcome="success")) is False


def test_edge_matches_contains_operator():
    result = Result(outcome="success", metadata={"test_failures": "critical,blocker"})
    edge_match = Edge(src="a", dst="b", attrs={"condition": "test_failures contains critical"})
    edge_no_match = Edge(src="a", dst="b", attrs={"condition": "test_failures contains missing"})
    edge_not_contains = Edge(src="a", dst="b", attrs={"condition": "test_failures not contains missing"})
    assert _edge_matches(edge_match, result) is True
    assert _edge_matches(edge_no_match, result) is False
    assert _edge_matches(edge_not_contains, result) is True


def test_edge_matches_in_operator():
    result = Result(outcome="success", metadata={"error_code": "404"})
    edge_match = Edge(src="a", dst="b", attrs={"condition": "error_code in '404, 500'"})
    edge_no_match = Edge(src="a", dst="b", attrs={"condition": "error_code in '200, 301'"})
    assert _edge_matches(edge_match, result) is True
    assert _edge_matches(edge_no_match, result) is False


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
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo", cxdb_path=db_path)
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
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")
    history = run(g, ctx, checkpoint=checkpoint, max_steps=50)

    saved = json.loads(checkpoint.read_text())
    assert history[-1].outcome == "exhausted"
    assert saved[-1]["outcome"] == "exhausted"
    assert len(saved) == len(history)


def test_prompt_references_cannot_escape_workdir():
    # Build a path that is within the configured holdouts directory so the
    # check is correct regardless of platform or DARK_FACTORY_HOLDOUTS value.
    holdout_path = _holdouts_repo_path() / "holdouts" / "hello" / "scenarios.yaml"
    node = Node(
        name="leak",
        attrs={"prompt": f"@{holdout_path}"},
    )
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")

    text = _render_prompt(node, ctx)

    assert "expect_return" not in text
    assert "Hello, world!" not in text
    assert "invalid prompt" in text


def test_prompt_references_allow_absolute_prompt_path(tmp_path):
    prompt_file = tmp_path / "absolute_prompt.md"
    prompt_file.write_text("Print exactly: done")

    node = Node(
        name="implement",
        attrs={"prompt": f"@{prompt_file}"},
    )
    ctx = Context(goal="short", workdir=SCRATCH, backend="echo")
    text = _render_prompt(node, ctx)

    assert text == "Print exactly: done"


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

    scenarios = str(
        SEALED_HOLDOUTS_REPO / "holdouts" / "hello" / "scenarios.yaml"
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
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")

    result = _tool(node, ctx)

    assert result.outcome == "failure"
    assert "expect_return" not in result.output
    assert "Hello, world!" not in result.output


def test_tool_sandbox_still_denies_real_holdouts_when_env_overridden(monkeypatch, tmp_path):
    from runner.handlers import _tool

    fake_holdouts = tmp_path / "fake-holdouts"
    fake_holdouts.mkdir()
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(fake_holdouts))
    scenarios = str(
        SEALED_HOLDOUTS_REPO / "holdouts" / "hello" / "scenarios.yaml"
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
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")

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

    ctx = Context(
        goal="t",
        workdir=tmp_path,
        backend="ao",
        state={"ao.project": "dark-factory", "ao.agent": "antigravity"},
    )
    result = _codergen(Node(name="implement", attrs={"prompt": "@prompt.md"}), ctx)

    assert result.outcome == "success"
    assert commands
    assert commands[0][:3] == ["sandboxed", "ao", "spawn"]
    assert commands[0][3] == "do work"
    assert "--project" in commands[0]
    assert commands[0][commands[0].index("--project") + 1] == "dark-factory"
    assert "--agent" in commands[0]
    assert commands[0][commands[0].index("--agent") + 1] == "antigravity"
    assert "--prompt" not in commands[0]
    assert "--harness" not in commands[0]



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
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")

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
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")

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
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")

    result = _holdout_eval(node, ctx)

    assert result.outcome != "success"


def test_holdout_eval_subprocess_env_is_sanitized(monkeypatch, tmp_path):
    """The eval subprocess env must not carry DARK_FACTORY_HOLDOUTS or any
    *HOLDOUT* variable — the same eval_env dict feeds agent-authored server
    and seed subprocesses (make run, npm seed, scripts/seed.*), which is a
    holdout exfiltration vector (jleechan-4pa / issue #29)."""
    fake_repo = tmp_path / "fake-holdouts"
    evaluator = fake_repo / "evaluator"
    evaluator.mkdir(parents=True)
    # The fake evaluator passes only if its own env is clean: a leak flips
    # the verdict to fail, which the assertion below catches.
    (evaluator / "run.py").write_text(
        "import json, os\n"
        "leaked = [k for k in os.environ if 'HOLDOUT' in k.upper()]\n"
        "verdict = 'fail' if leaked else 'pass'\n"
        "print(json.dumps({'verdict': verdict, 'scenarios': [], 'leaked': leaked}))\n"
    )
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(fake_repo))
    monkeypatch.setenv("MY_HOLDOUT_SECRET", "sealed")

    node = Node(name="holdout", attrs={"type": "holdout_eval", "feature": "hello"})
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")

    result = _holdout_eval(node, ctx)

    assert result.outcome == "success", (
        f"holdout vars leaked into the eval subprocess env: {result.output}"
    )


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
    ctx = Context(goal="t", workdir=SCRATCH, backend="echo")
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


# ---------------------------------------------------------------------------
# Cross-run exhaustion circuit breaker (v4 hardening)
# ---------------------------------------------------------------------------
#
# Prevents the 2026-06-27 failure mode where 16+ WIP-exhausted commits stack
# on the same branch — each fresh run re-executes explore → plan → implement
# → test → fix and exhausts again, because the underlying gate cannot be
# passed by anything the in-loop reviewer can fix.
#
# Proof state (per root-cause-first):
#   - Server-owned invariant: CXDB stores cross-run state that the agent
#     cannot see. Engine owns run lifecycle.
#   - Prompt-insufficient (proven): the fix prompt has no signal about
#     prior-run exhaustion; agent is asked to fix without awareness of
#     the failure streak.
#
# The breaker fires at run start when the last N runs of the same pipeline
# all ended with `final='exhausted'`. It emits a synthetic exhausted record
# and skips all node execution so the operator sees the unrecoverable state
# in CXDB rather than burning LLM budget on a guaranteed-to-fail attempt.

_CB_THRESHOLD = 3  # matches the documented streak from memory 2026-06-27


def test_cross_run_circuit_breaker_short_circuits_when_prior_runs_all_exhausted(
    monkeypatch, tmp_path
):
    """When the last 3 prior runs of the same pipeline all ended with
    ``final='exhausted'``, the engine must emit a synthetic exhausted record
    at run start without invoking any node handler.
    """
    cxdb_path = tmp_path / "cxdb.sqlite"

    # 1. Seed CXDB with 3 prior runs that all ended exhausted for pipeline "hello".
    seed = CXDB(cxdb_path)
    try:
        for _ in range(_CB_THRESHOLD):
            rid = seed.start_run(pipeline="hello", goal="seed")
            seed.record_step(
                run_id=rid, seq=0, node="start", outcome="success",
                ts=0.0, output="start", metadata={},
            )
            seed.record_step(
                run_id=rid, seq=1, node="fix", outcome="exhausted",
                ts=0.1, output="max_visits=3 exceeded",
                metadata={"max_visits": "3"},
            )
            seed.end_run(rid, "exhausted")
    finally:
        seed.close()

    # 2. Mock holdout_eval to track invocations. If the engine reaches the
    # holdout node, this counter increments. The circuit breaker must
    # short-circuit BEFORE any handler is called.
    handler_calls = {"count": 0}

    def fake_holdout(node, ctx):
        handler_calls["count"] += 1
        return Result(outcome="success", output="mock pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    # 3. Run the engine with the seeded CXDB attached.
    g = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="circuit breaker test",
        workdir=SCRATCH,
        backend="echo",
        cxdb_path=cxdb_path,
    )
    history = run(g, ctx, max_steps=50)

    # 4. Assert: engine short-circuited without running any node handler.
    assert handler_calls["count"] == 0, (
        f"expected 0 handler invocations, got {handler_calls['count']}"
    )
    assert len(history) == 1, (
        f"expected exactly 1 record, got {len(history)}: {[r.node for r in history]}"
    )
    assert history[0].outcome == "exhausted"
    assert history[0].node == "__cross_run_circuit__"
    assert history[0].metadata.get("cross_run_circuit_breaker") == "true"


def test_cross_run_circuit_breaker_disabled_when_threshold_not_reached(
    monkeypatch, tmp_path
):
    """With fewer than 3 prior exhausted runs, the engine must NOT
    short-circuit — normal pipeline execution proceeds."""
    cxdb_path = tmp_path / "cxdb.sqlite"

    # Seed only 2 prior exhausted runs (threshold is 3)
    seed = CXDB(cxdb_path)
    try:
        for _ in range(_CB_THRESHOLD - 1):
            rid = seed.start_run(pipeline="hello", goal="seed")
            seed.record_step(
                run_id=rid, seq=0, node="start", outcome="success",
                ts=0.0, output="start", metadata={},
            )
            seed.end_run(rid, "exhausted")
    finally:
        seed.close()

    monkeypatch.setitem(
        TYPE_REGISTRY, "holdout_eval",
        lambda node, ctx: Result(outcome="success", output="mock pass"),
    )

    g = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="below threshold test",
        workdir=SCRATCH,
        backend="echo",
        cxdb_path=cxdb_path,
    )
    history = run(g, ctx, max_steps=50)

    # Should run normally — holdout succeeds, exit reached
    assert history[-1].outcome == "success"
    assert history[-1].node == "exit"


def test_cross_run_circuit_breaker_breaks_streak_on_success(monkeypatch, tmp_path):
    """If ANY of the last N runs was not exhausted, the breaker must NOT fire."""
    cxdb_path = tmp_path / "cxdb.sqlite"

    seed = CXDB(cxdb_path)
    try:
        # 2 exhausted → 1 success → 2 exhausted. Most recent 3 = success+2xexhausted
        # → streak is broken, breaker must not fire.
        for _ in range(2):
            rid = seed.start_run(pipeline="hello", goal="seed")
            seed.record_step(
                run_id=rid, seq=0, node="start", outcome="success",
                ts=0.0, output="start", metadata={},
            )
            seed.end_run(rid, "exhausted")
        rid_success = seed.start_run(pipeline="hello", goal="seed-success")
        seed.record_step(
            run_id=rid_success, seq=0, node="start", outcome="success",
            ts=0.0, output="start", metadata={},
        )
        seed.end_run(rid_success, "success")
        for _ in range(2):
            rid = seed.start_run(pipeline="hello", goal="seed")
            seed.record_step(
                run_id=rid, seq=0, node="start", outcome="success",
                ts=0.0, output="start", metadata={},
            )
            seed.end_run(rid, "exhausted")
    finally:
        seed.close()

    monkeypatch.setitem(
        TYPE_REGISTRY, "holdout_eval",
        lambda node, ctx: Result(outcome="success", output="mock pass"),
    )

    g = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="streak broken test",
        workdir=SCRATCH,
        backend="echo",
        cxdb_path=cxdb_path,
    )
    history = run(g, ctx, max_steps=50)

    # Should run normally — streak is broken by the success in between
    assert history[-1].outcome == "success"
    assert history[-1].node == "exit"


def test_cross_run_circuit_breaker_skipped_without_cxdb(monkeypatch, tmp_path):
    """Without CXDB attached, the circuit breaker must not fire — runs proceed
    normally. The breaker is opt-in via CXDB presence."""
    monkeypatch.setitem(
        TYPE_REGISTRY, "holdout_eval",
        lambda node, ctx: Result(outcome="success", output="mock pass"),
    )

    g = parse(_pipeline("hello.dot"))
    # No cxdb_path
    ctx = Context(goal="no cxdb test", workdir=SCRATCH, backend="echo")
    history = run(g, ctx, max_steps=50)

    assert history[-1].outcome == "success"
    assert history[-1].node == "exit"


def test_cross_run_circuit_breaker_only_affects_matching_pipeline(
    monkeypatch, tmp_path
):
    """Streak on pipeline A must NOT short-circuit pipeline B."""
    cxdb_path = tmp_path / "cxdb.sqlite"

    seed = CXDB(cxdb_path)
    try:
        # 3 exhausted runs on pipeline "other-pipeline" — different from "hello"
        for _ in range(_CB_THRESHOLD):
            rid = seed.start_run(pipeline="other-pipeline", goal="seed")
            seed.record_step(
                run_id=rid, seq=0, node="start", outcome="success",
                ts=0.0, output="start", metadata={},
            )
            seed.end_run(rid, "exhausted")
    finally:
        seed.close()

    handler_calls = {"count": 0}

    def fake_holdout(node, ctx):
        handler_calls["count"] += 1
        return Result(outcome="success", output="mock pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="pipeline mismatch test",
        workdir=SCRATCH,
        backend="echo",
        cxdb_path=cxdb_path,
    )
    history = run(g, ctx, max_steps=50)

    # Different pipeline — breaker must not fire; pipeline runs to completion
    assert handler_calls["count"] >= 1
    assert history[-1].outcome == "success"
    assert history[-1].node == "exit"


# ---------------------------------------------------------------------------
# Cross-run circuit breaker — time decay / quota classification (rev-vl3zr)
# ---------------------------------------------------------------------------
#
# ROOT-CAUSE: the v4 breaker above treats "3 consecutive exhausted runs
# because of a transient upstream quota exhaustion" identically to "3
# genuinely-stuck runs" — once tripped, it blocks forever even after the
# quota resets. Fix: decay the effective streak count by half for every
# CB_DECAY_HALF_LIFE_SECS (default 30 min) of idle time since the most
# recent exhausted run, so a long-enough gap unblocks dispatch again.

from runner.engine_run import (  # noqa: E402
    CB_DECAY_HALF_LIFE_SECS,
    _decayed_exhausted_streak,
)


def test_decayed_exhausted_streak_unchanged_at_zero_idle():
    """No idle time → no decay; the raw streak count is returned."""
    now = 1_000_000.0
    assert _decayed_exhausted_streak(3, now, now=now) == pytest.approx(3.0)


def test_decayed_exhausted_streak_halves_after_one_half_life():
    """Exactly one half-life of idle time (default 30 min) halves the streak —
    matches the ACCEPTANCE wording verbatim: '30 min of idle reduces
    exhausted_streak by half'."""
    now = 1_000_000.0
    most_recent_ended_ts = now - CB_DECAY_HALF_LIFE_SECS
    assert _decayed_exhausted_streak(3, most_recent_ended_ts, now=now) == pytest.approx(1.5)


def test_decayed_exhausted_streak_quarters_after_two_half_lives():
    now = 1_000_000.0
    most_recent_ended_ts = now - (2 * CB_DECAY_HALF_LIFE_SECS)
    assert _decayed_exhausted_streak(3, most_recent_ended_ts, now=now) == pytest.approx(0.75)


def test_decayed_exhausted_streak_noop_when_no_timestamp():
    assert _decayed_exhausted_streak(3, None) == 3.0


def test_decayed_exhausted_streak_noop_when_disabled(monkeypatch):
    monkeypatch.setattr("runner.engine_run.CB_DECAY_HALF_LIFE_SECS", 0)
    now = 1_000_000.0
    assert _decayed_exhausted_streak(3, now - 999999, now=now) == 3.0


def test_cross_run_circuit_breaker_does_not_block_after_quota_reset_idle_gap(
    monkeypatch, tmp_path
):
    """ACCEPTANCE: after 3 consecutive exhausted runs due to quota, the 4th
    dispatch is NOT blocked if quota reset happened in between (i.e. a long
    idle gap has elapsed since the last exhausted run)."""
    cxdb_path = tmp_path / "cxdb.sqlite"

    # 3 prior exhausted runs, all ended long enough ago (2x half-life) that
    # the effective streak has decayed below CB_THRESHOLD.
    stale_ended_ts = time.time() - (2 * CB_DECAY_HALF_LIFE_SECS)
    seed = CXDB(cxdb_path)
    try:
        for _ in range(_CB_THRESHOLD):
            rid = seed.start_run(pipeline="hello", goal="seed")
            seed.record_step(
                run_id=rid, seq=0, node="start", outcome="success",
                ts=0.0, output="start", metadata={},
            )
            seed.end_run(rid, "exhausted", ended_ts=stale_ended_ts)
    finally:
        seed.close()

    handler_calls = {"count": 0}

    def fake_holdout(node, ctx):
        handler_calls["count"] += 1
        return Result(outcome="success", output="mock pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="quota reset idle gap test",
        workdir=SCRATCH,
        backend="echo",
        cxdb_path=cxdb_path,
    )
    history = run(g, ctx, max_steps=50)

    # The streak has decayed below threshold — the 4th dispatch must proceed
    # to normal execution instead of short-circuiting.
    assert handler_calls["count"] >= 1, (
        "expected the run to proceed past the decayed circuit breaker, "
        f"but no handler was invoked; history={[r.node for r in history]}"
    )
    assert history[-1].outcome == "success"
    assert history[-1].node == "exit"


def test_cross_run_circuit_breaker_still_fires_without_idle_gap(monkeypatch, tmp_path):
    """Regression guard: with no meaningful idle time (runs ended just now),
    the breaker must still fire — decay must not defeat the original v4
    protection for genuinely back-to-back exhaustion."""
    cxdb_path = tmp_path / "cxdb.sqlite"

    seed = CXDB(cxdb_path)
    try:
        for _ in range(_CB_THRESHOLD):
            rid = seed.start_run(pipeline="hello", goal="seed")
            seed.record_step(
                run_id=rid, seq=0, node="start", outcome="success",
                ts=0.0, output="start", metadata={},
            )
            seed.end_run(rid, "exhausted")  # ended_ts defaults to now
    finally:
        seed.close()

    handler_calls = {"count": 0}

    def fake_holdout(node, ctx):
        handler_calls["count"] += 1
        return Result(outcome="success", output="mock pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)

    g = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="no idle gap regression test",
        workdir=SCRATCH,
        backend="echo",
        cxdb_path=cxdb_path,
    )
    history = run(g, ctx, max_steps=50)

    assert handler_calls["count"] == 0
    assert len(history) == 1
    assert history[0].outcome == "exhausted"
    assert history[0].node == "__cross_run_circuit__"
    assert history[0].metadata.get("cross_run_circuit_breaker") == "true"
    # New observability fields proving decay was evaluated, not skipped.
    assert "effective_streak" in history[0].metadata
    assert float(history[0].metadata["effective_streak"]) >= _CB_THRESHOLD
