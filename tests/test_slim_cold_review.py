"""Focused contract tests for the compact native cold-review fallback."""

from __future__ import annotations

import json
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
    from runner.handler_core import Result
    from runner.handler_parallel_reviewer import _contract_adjusted_result

    request = create_review_request(_inputs())
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._verify_controller_workspace",
        lambda ctx, request: None,
    )
    result = _contract_adjusted_result(
        Result(outcome="success", output=_compact_response()),
        request,
        object(),
        lane="primary",
    )
    assert result.outcome == "failure"
    assert "successful command receipt" in result.metadata["review_contract_gap"]


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
