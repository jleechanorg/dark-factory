"""Safety contracts for workflow_graphgen executable defaults."""

from benchmarks.workflow_graphgen.generator import deterministic_generator
from benchmarks.workflow_graphgen.graph_ir import render_middle_only_dot
import benchmarks.workflow_graphgen.__main__ as cli


def test_deterministic_generator_defaults_to_non_claude_without_model():
    ir = deterministic_generator("safe defaults")
    assert {node.backend for node in ir.nodes} == {"codex"}
    assert {node.model_name for node in ir.nodes} == {None}
    dot = render_middle_only_dot(ir)
    assert 'backend="claude"' not in dot
    assert 'backend="claude-sonnet"' not in dot
    assert 'model_name="claude-sonnet-4-6"' not in dot


def test_explicit_claude_opt_in_remains_supported_and_scoped():
    ir = deterministic_generator(
        "explicit opt in", backend="claude", model_name="claude-sonnet-4-6"
    )
    dot = render_middle_only_dot(ir)
    assert 'backend="claude"' in dot
    assert 'model_name="claude-sonnet-4-6"' in dot
    assert dot.count('explicit_claude_lane="true"') == 2
    assert dot.count('requires_claude_config="true"') == 2


def test_cli_defaults_are_safe_and_explicit_opt_in_is_forwarded(tmp_path, monkeypatch):
    calls = []

    def fake_run_benchmark(**kwargs):
        calls.append(kwargs)
        return []

    monkeypatch.setattr(cli, "run_benchmark", fake_run_benchmark)
    monkeypatch.setattr(cli, "aggregate", lambda records: {"results": []})
    out = tmp_path / "records.jsonl"
    assert cli.main(["--out", str(out)]) == 0
    assert calls[-1]["backend"] == "codex"
    assert calls[-1]["model_name"] is None

    assert cli.main([
        "--backend", "claude", "--model", "claude-sonnet-4-6",
        "--out", str(out),
    ]) == 0
    assert calls[-1]["backend"] == "claude"
    assert calls[-1]["model_name"] == "claude-sonnet-4-6"
