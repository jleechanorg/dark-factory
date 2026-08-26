"""Focused contract tests for the compact native cold-review fallback."""

from __future__ import annotations

import json
import subprocess
from dataclasses import replace
from pathlib import Path

import pytest
from test_review_controller import _inputs

from runner.review_controller import (
    CHECK_IDS,
    ExecutionReceipt,
    ReviewContractError,
    create_review_request,
    run_controller_review,
    validate_execution_receipts,
    validate_review_response,
)


def _compact_response(*, verdict: str = "pass", **overrides: object) -> str:
    payload: dict[str, object] = {
        "verdict": verdict,
        "findings": [],
        "evidence_checked": ["bound source tree and focused tests"],
        "commands_executed": ["python -m pytest -q"],
        "caveats": [],
    }
    payload.update(overrides)
    return json.dumps(payload, separators=(",", ":"))


def test_compact_contract_has_no_checklist_or_controller_hash_echoes() -> None:
    request = create_review_request(_inputs())
    assert CHECK_IDS == ()
    assert len(request.prompt_payload) < 5000
    assert "C0" not in request.prompt
    assert "E14" not in request.prompt
    assert "PROMPT_SHA256:" not in request.prompt
    assert "ENVELOPE_SHA256:" not in request.prompt
    assert "HEAD_SHA:" not in request.prompt
    assert "TASK_SHA256:" not in request.prompt
    assert "CHANGED_FILES_SHA256:" not in request.prompt
    assert "EVIDENCE_MANIFEST_SHA256:" not in request.prompt
    for phrase in (
        "untrusted",
        "callers",
        "security",
        "state",
        "boundary",
        "evidence",
        "read-only",
        "uncertainty",
        "continue",
    ):
        assert phrase in request.prompt_payload.lower()


def test_compact_response_is_strict_json_with_empty_checks_compatibility() -> None:
    request = create_review_request(_inputs())
    result = validate_review_response(_compact_response(), request)
    assert result.verdict == "pass"
    assert result.checks == ()
    assert len(result.response_sha256) == 64


def test_pass_requires_meaningful_evidence_and_commands() -> None:
    request = create_review_request(_inputs())
    response = _compact_response(evidence_checked=[], commands_executed=[])
    with pytest.raises(ReviewContractError, match="pass requires"):
        validate_review_response(response, request)


def test_pass_requires_a_captured_successful_command_receipt() -> None:
    request = create_review_request(_inputs())
    validated = validate_review_response(_compact_response(), request)
    with pytest.raises(ReviewContractError, match="successful command receipt"):
        validate_execution_receipts((), validated)


def test_pass_uses_authoritative_receipts_with_human_command_summary() -> None:
    request = create_review_request(_inputs())
    validated = validate_review_response(
        _compact_response(
            commands_executed=["ran a probe, then the focused verification suite"],
        ),
        request,
    )
    receipts = (
        ExecutionReceipt(
            command="probe command",
            exit_code=1,
            output_sha256="1" * 64,
        ),
        ExecutionReceipt(
            command="focused verification command",
            exit_code=0,
            output_sha256="2" * 64,
        ),
    )
    validate_execution_receipts(receipts, validated)


def test_controller_acceptance_rejects_pass_without_captured_receipt(monkeypatch) -> None:
    from runner.handler_core import Context, Result
    from runner.handler_parallel_reviewer import _contract_adjusted_result

    request = create_review_request(_inputs())
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._verify_controller_workspace",
        lambda ctx, request: None,
    )
    result = _contract_adjusted_result(
        Result(outcome="success", output=_compact_response()),
        request,
        Context(goal="", workdir=Path(".")),
        lane="primary",
    )
    assert result.outcome == "failure"
    assert "successful command receipt" in result.metadata["review_contract_gap"]



@pytest.mark.parametrize(
    ("backend", "expected_outcome"),
    (("codex", "failure"), ("echo", "success"), ("mock_llm", "success")),
)
def test_graph_controller_stub_pass_depends_on_backend(
    monkeypatch, backend, expected_outcome
) -> None:
    from runner.handler_core import Context, Result
    from runner.handler_parallel_reviewer import _contract_adjusted_result

    request = create_review_request(_inputs())
    monkeypatch.setenv("DARK_FACTORY_ITERATION_STUB", "1")
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._verify_controller_workspace",
        lambda ctx, request: None,
    )
    result = _contract_adjusted_result(
        Result(
            outcome="success",
            output=_compact_response(),
            metadata={
                "_controller_command_receipts": [
                    {
                        "command": "pytest",
                        "exit_code": 0,
                        "output_sha256": "0" * 64,
                    }
                ]
            },
        ),
        request,
        Context(goal="", workdir=Path("."), backend=backend),
        lane="primary",
        backend=backend,
    )

    assert result.outcome == expected_outcome
    if backend == "codex":
        assert "stub-mode" in result.metadata["review_contract_gap"]


def test_clean_worker_uses_a_detached_controller_snapshot(tmp_path, monkeypatch):
    from test_review_cli import _repo

    from runner.handler_parallel_reviewer import _controller_snapshot

    repo, _, head = _repo(tmp_path)
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer.pathlib.Path.home",
        lambda: tmp_path,
    )
    snapshot, snapshot_head, origin = _controller_snapshot(repo, head, ())

    try:
        assert snapshot != repo
        assert snapshot_head == head
        assert origin is None
        assert subprocess.run(
            ["git", "-C", str(snapshot), "symbolic-ref", "-q", "HEAD"],
            check=False,
        ).returncode == 1
        assert subprocess.run(
            ["git", "-C", str(snapshot), "status", "--porcelain=v1"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout == ""
    finally:
        subprocess.run(
            ["git", "-C", str(repo), "worktree", "remove", "--force", str(snapshot)],
            check=True,
        )


def test_controller_acceptance_binds_detached_target_and_exit_repins(
    tmp_path,
) -> None:
    from test_review_cli import _repo

    from runner.handler_core import Context, Result, _exit
    from runner.handler_parallel_reviewer import _contract_adjusted_result
    from runner.parser import Node

    repo, base, head = _repo(tmp_path)
    snapshot = tmp_path / "detached-review-snapshot"
    subprocess.run(
        ["git", "-C", str(repo), "worktree", "add", "--detach", str(snapshot), head],
        check=True,
        capture_output=True,
        text=True,
    )
    tree = subprocess.check_output(
        ["git", "-C", str(snapshot), "rev-parse", "HEAD^{tree}"],
        text=True,
    ).strip()
    request = create_review_request(
        replace(
            _inputs(),
            workspace_path=str(snapshot),
            base_sha=base,
            head_sha=head,
            tree_sha=tree,
            evidence=(),
        )
    )
    result = _contract_adjusted_result(
        Result(
            outcome="success",
            output=_compact_response(),
            metadata={
                "_controller_command_receipts": [
                    {
                        "command": "pytest",
                        "exit_code": 0,
                        "output_sha256": "0" * 64,
                    }
                ]
            },
        ),
        request,
        Context(goal="", workdir=repo),
        lane="primary",
        backend="codex",
    )

    assert result.outcome == "success", result.metadata
    binding = json.loads(result.context_updates["_verified_review_target"])
    assert binding == {
        "workspace_path": str(snapshot),
        "head_sha": head,
        "tree_sha": tree,
    }
    ctx = Context(goal="", workdir=repo, state=dict(result.context_updates))
    assert _exit(Node(name="exit", attrs={}), ctx).outcome == "success"

    subprocess.run(
        ["git", "-C", str(snapshot), "checkout", "--quiet", "--detach", base],
        check=True,
    )
    changed = _exit(Node(name="exit", attrs={}), ctx)
    assert changed.outcome == "error"
    assert changed.metadata["exit_sha_status"] == "mismatched"


@pytest.mark.parametrize(
    "binding",
    (
        "not-json",
        "[]",
        json.dumps({"workspace_path": "/tmp", "head_sha": "a" * 40}),
        json.dumps(
            {"workspace_path": 7, "head_sha": "a" * 40, "tree_sha": "b" * 40}
        ),
    ),
)
def test_contract_exit_fails_closed_on_malformed_binding(binding) -> None:
    from runner.handler_core import Context, _exit
    from runner.parser import Node

    result = _exit(
        Node(name="exit", attrs={}),
        Context(
            goal="",
            workdir=Path("."),
            state={
                "_verified_review_target_required": "true",
                "_verified_review_target": binding,
            },
        ),
    )

    assert result.outcome == "error"
    assert result.metadata["exit_sha_status"] == "invalid"


def test_contract_exit_requires_binding_when_controller_marked_it_required() -> None:
    from runner.handler_core import Context, _exit
    from runner.parser import Node

    result = _exit(
        Node(name="exit", attrs={}),
        Context(
            goal="",
            workdir=Path("."),
            state={"_verified_review_target_required": "true"},
        ),
    )

    assert result.outcome == "error"
    assert "binding is missing" in result.output


def test_contract_exit_rejects_branch_target(tmp_path) -> None:
    from test_review_cli import _repo

    from runner.handler_core import Context, _exit
    from runner.parser import Node

    repo, _, head = _repo(tmp_path)
    tree = subprocess.check_output(
        ["git", "-C", str(repo), "rev-parse", "HEAD^{tree}"],
        text=True,
    ).strip()
    binding = json.dumps(
        {"workspace_path": str(repo), "head_sha": head, "tree_sha": tree},
        sort_keys=True,
        separators=(",", ":"),
    )
    result = _exit(
        Node(name="exit", attrs={}),
        Context(
            goal="",
            workdir=repo,
            state={
                "_verified_review_target_required": "true",
                "_verified_review_target": binding,
            },
        ),
    )

    assert result.outcome == "error"
    assert "detached snapshot" in result.output


def test_fail_response_remains_valid_without_evidence_or_receipts() -> None:
    request = create_review_request(_inputs())
    validated = validate_review_response(
        _compact_response(
            verdict="fail",
            findings=["material uncertainty"],
            evidence_checked=[],
            commands_executed=[],
        ),
        request,
    )
    validate_execution_receipts((), validated)


def test_two_node_controller_requires_receipt() -> None:
    from runner.parser import parse

    graph = parse(Path(__file__).parents[1] / "pipelines/slim/two_node.dot")
    assert graph.nodes["cold_reviewer"].attrs.get("receipt_required") == "true"


@pytest.mark.parametrize(
    "payload",
    (
        {"verdict": "pass", "findings": [], "evidence_checked": [], "commands_executed": []},
        {
            "verdict": "pass",
            "findings": [],
            "evidence_checked": [],
            "commands_executed": [],
            "caveats": [],
            "extra": "reject",
        },
        {
            "verdict": "PASS",
            "findings": [],
            "evidence_checked": [],
            "commands_executed": [],
            "caveats": [],
        },
        {
            "verdict": "pass",
            "findings": "not a list",
            "evidence_checked": [],
            "commands_executed": [],
            "caveats": [],
        },
        '{"verdict":"pass","verdict":"fail","findings":[],"evidence_checked":[],"commands_executed":[],"caveats":[]}',
    ),
)
def test_compact_response_rejects_non_exact_schema(payload: dict[str, object] | str) -> None:
    request = create_review_request(_inputs())
    with pytest.raises(ReviewContractError):
        validate_review_response(json.dumps(payload), request)


def test_nonzero_reviewer_exit_is_rejected_before_transport_parsing(monkeypatch, tmp_path) -> None:
    request = create_review_request(_inputs())

    class _FailedProcess:
        returncode = 23
        stdout = "not JSONL at all"
        stderr = "review backend exploded"

    calls: list[object] = []

    def fake_run(*args, **kwargs):
        calls.append((args, kwargs))
        return _FailedProcess()

    monkeypatch.setattr("runner.review_controller.subprocess.run", fake_run)
    with pytest.raises(ReviewContractError, match="exited with 23"):
        run_controller_review(
            request,
            neutral_cwd=tmp_path,
            output_dir=tmp_path / "review-output",
            transport_argv=("fake-reviewer",),
            timeout=10,
        )
    assert calls
