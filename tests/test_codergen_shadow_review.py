from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402
from runner.handlers import Context, _codergen  # noqa: E402
from runner.subprocess_control import BoundedProcessResult  # noqa: E402


class _FakePopen:
    pid = 12345

    def __init__(self, *args, **kwargs):
        self.args = args[0]
        self.returncode = 0

    def communicate(self, timeout=None):
        return (
            "## Review Verdict\nfail\n\n"
            "## Blocking Findings\n"
            "1. Severity: blocker\n"
            "   Evidence: tests/proof.txt\n"
            "   Why it matters: coder needs this exact finding.\n"
            "   Fix: update implementation.\n\n"
            "verdict: fail\n",
            "",
        )


def test_review_codergen_runs_parallel_codex_shadow_review(tmp_path, monkeypatch):
    prompt = tmp_path / "review.md"
    prompt.write_text("primary reviewer prompt for ${goal}\n", encoding="utf-8")
    event_log = tmp_path / "events.jsonl"
    node = make_node(
        name="review",
        type="codergen",
        backend="codex",
        class_="review",
        prompt=f"@{prompt}",
    )
    # DOT attrs use `class`, but Python keyword syntax needs a manual patch.
    node.attrs["class"] = "review"
    node.attrs.pop("class_", None)
    ctx = Context(
        goal="shadow review proof",
        workdir=tmp_path,
        backend="codex",
        run_id="shadowreview1",
        event_log_path=event_log,
    )
    ctx.state["_last_diff"] = "diff --git a/demo.py b/demo.py"
    ctx.state["_last_changed_files"] = "- demo.py"
    ctx.state["_df_shadow_codex_review"] = "true"

    monkeypatch.setattr(
        "runner.codex_runtime.resolve_codex_executable",
        lambda: "/test-fixtures/.nvm/versions/node/v22.22.0/bin/codex",
    )
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: args)
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _FakePopen)

    def _fake_run(args, **kwargs):
        if args and args[0] == "git":
            return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
        assert pathlib.Path(args[0]).name == "codex"
        assert args[1:4] == ["exec", "--yolo", "--skip-git-repo-check"]
        return subprocess.CompletedProcess(
            args, 0, stdout="primary reviewer says pass\n", stderr=""
        )

    def _fake_bounded(args, **kwargs):
        completed = _fake_run(args, **kwargs)
        return BoundedProcessResult(
            args=tuple(args),
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
            timed_out=False,
        )

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_codergen.run_bounded_process", _fake_bounded)

    result = _codergen(node, ctx)

    assert result.outcome == "failure"
    assert "primary reviewer says pass" in result.output
    assert "## Parallel Codex Review" in result.output
    assert "coder needs this exact finding" in result.output
    assert result.metadata["shadow_codex_review"] == "true"
    assert result.metadata["shadow_codex_outcome"] == "failure"
    assert result.metadata["shadow_codex_verdict"] == "fail"
    assert result.context_updates["review.shadow_codex_outcome"] == "failure"
    assert pathlib.Path(result.metadata["shadow_codex_prompt_path"]).exists()
    assert pathlib.Path(result.metadata["shadow_codex_output_path"]).exists()
    assert "review this diff" in pathlib.Path(result.metadata["shadow_codex_prompt_path"]).read_text()
    assert "coder needs this exact finding" in pathlib.Path(result.metadata["shadow_codex_output_path"]).read_text()
