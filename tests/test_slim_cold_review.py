"""Focused contract tests for the compact native cold-review fallback."""

from __future__ import annotations

import json

import pytest
from test_review_controller import _inputs

from runner.review_controller import (
    CHECK_IDS,
    ReviewContractError,
    create_review_request,
    run_controller_review,
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
