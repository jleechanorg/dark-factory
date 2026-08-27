"""Controller transport must sandbox the envelope target, not the source checkout."""

from __future__ import annotations

from pathlib import Path

from runner.handler_core import Context, Result
from runner.handler_dispatch import (
    _build_controller_codex_transport,
    _launch_shadow_gate_review,
    _ShadowGateReview,
)
from runner.handler_parallel_reviewer import _parallel_reviewer
from runner.parser import Node
from runner.review_controller import ReviewInputs, create_review_request


def _request(snapshot: Path):
    sha = "a" * 40
    return create_review_request(
        ReviewInputs(
            repository="example/repo",
            workspace_path=str(snapshot),
            base_sha=sha,
            head_sha=sha,
            tree_sha="b" * 40,
            task_text="Review the change.",
        )
    )


def _sandboxed_codex_args() -> list[str]:
    return [
        "/usr/bin/sandbox-exec",
        "-p",
        (
            '(version 1)\n(allow default)\n'
            '(deny file-read* (subpath "/sealed/holdouts"))\n'
        ),
        "/usr/local/bin/codex",
        "exec",
        "--yolo",
        "--skip-git-repo-check",
        "ignored prompt",
    ]


def _assert_snapshot_profile(transport: list[str], source: Path, snapshot: Path) -> None:
    profile = transport[2]
    assert f'(deny file-write* (subpath "{snapshot}"))' in profile
    assert str(source) not in profile
    assert "--dangerously-bypass-approvals-and-sandbox" in transport
    assert "--sandbox" not in transport


def test_graph_primary_uses_validated_envelope_snapshot_for_write_denial(
    tmp_path, monkeypatch
):
    source = tmp_path / "source"
    snapshot = tmp_path / "snapshot"
    source.mkdir()
    snapshot.mkdir()
    request = _request(snapshot)
    seen: dict[str, Path] = {}

    def fake_primary(*args, **kwargs):
        seen["read_only_path"] = Path(kwargs["read_only_path"])
        return Result(outcome="success", output="controller response")

    node = Node(
        name="cold_reviewer",
        attrs={"review_contract": "cold-review-v1", "backend": "codex"},
    )
    ctx = Context(goal="review", workdir=source, backend="codex", run_id="target")
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._controller_review_request",
        lambda node, ctx, expected_sha: request,
    )
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda path: "a" * 40)
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._run_primary_review", fake_primary
    )
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._record_primary_output",
        lambda node, attempt, result, seq, ctx: result,
    )
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._contract_adjusted_result",
        lambda result, request, ctx, **kwargs: result,
    )

    result = _parallel_reviewer(node, ctx)

    assert result.outcome == "success"
    assert seen["read_only_path"] == snapshot.resolve()
    assert seen["read_only_path"] != source.resolve()
    transport = _build_controller_codex_transport(
        _sandboxed_codex_args(), read_only_path=seen["read_only_path"]
    )
    _assert_snapshot_profile(transport, source, snapshot.resolve())


def test_complete_prompt_shadow_uses_envelope_snapshot_for_write_denial(
    tmp_path, monkeypatch
):
    source = tmp_path / "source"
    snapshot = tmp_path / "snapshot"
    source.mkdir()
    snapshot.mkdir()
    seen: dict[str, object] = {}

    class FakePopen:
        pid = 123

        def __init__(self, command, **kwargs):
            seen["command"] = command

    monkeypatch.setattr("runner.handler_dispatch.sys.platform", "darwin")
    monkeypatch.setattr(
        "runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex"
    )
    monkeypatch.setattr(
        "runner.handler_dispatch._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: _sandboxed_codex_args(),
    )
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", FakePopen)

    review = _launch_shadow_gate_review(
        "cold_reviewer",
        "COMPLETE CONTROLLER PROMPT",
        "a" * 40,
        300,
        Context(goal="review", workdir=source, backend="codex"),
        prompt_is_complete=True,
        read_only_path=snapshot,
    )

    assert isinstance(review, _ShadowGateReview)
    transport = seen["command"]
    assert isinstance(transport, list)
    _assert_snapshot_profile(transport, source, snapshot.resolve())
