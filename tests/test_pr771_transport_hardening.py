"""Regression tests for controller transport isolation and exit status."""

from __future__ import annotations

import hashlib
import json
import pathlib

from runner.handler_core import Context, Result
from runner.handler_dispatch import _build_controller_codex_transport
from runner.handler_parallel_reviewer import _contract_adjusted_result
from runner.review_controller import (
    EvidenceArtifact,
    ReviewInputs,
    create_review_request,
)


def _request(tmp_path: pathlib.Path):
    sha = "a" * 40
    evidence = tmp_path / "evidence.txt"
    evidence.write_text("proof\n", encoding="utf-8")
    return create_review_request(
        ReviewInputs(
            repository="example",
            workspace_path=str(tmp_path),
            base_sha=sha,
            head_sha="b" * 40,
            tree_sha="c" * 40,
            task_text="Review the change.",
            changed_files=("evidence.txt",),
            evidence=(
                EvidenceArtifact(
                    path="evidence.txt",
                    size_bytes=evidence.stat().st_size,
                    sha256=hashlib.sha256(evidence.read_bytes()).hexdigest(),
                ),
            ),
        )
    )


def _valid_fail_response() -> str:
    return json.dumps(
        {
            "verdict": "fail",
            "findings": ["blocking finding"],
            "evidence_checked": ["evidence.txt"],
            "commands_executed": ["pytest -q"],
            "caveats": [],
        },
        separators=(",", ":"),
    )


def test_controller_transport_keeps_outer_holdout_sandbox_wrapper():
    """Native Codex read-only mode must retain path-specific outer denial."""
    sandboxed = [
        "/usr/bin/sandbox-exec",
        "-p",
        '(deny file-read* (subpath "/sealed/holdouts"))',
        "/usr/local/bin/codex",
        "exec",
        "--yolo",
        "--skip-git-repo-check",
        "ignored prompt",
    ]

    transport = _build_controller_codex_transport(sandboxed)

    assert transport[:3] == sandboxed[:3]
    assert transport[3:] == [
        "/usr/local/bin/codex",
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "-",
    ]
    assert '(deny file-read* (subpath "/sealed/holdouts"))' in transport


def test_controller_graph_rejects_nonzero_transport_with_valid_fail_response(
    tmp_path, monkeypatch
):
    """A valid fail payload cannot make a failed transport a valid review."""
    request = _request(tmp_path)
    monkeypatch.setattr(
        "runner.handler_parallel_reviewer._verify_controller_workspace",
        lambda ctx, req: None,
    )
    result = _contract_adjusted_result(
        Result(
            outcome="failure",
            output=_valid_fail_response(),
            metadata={"returncode": "7"},
        ),
        request,
        Context(goal="review", workdir=tmp_path),
        lane="primary",
        backend="codex",
    )

    assert result.outcome == "failure"
    assert result.metadata["review_contract_status"] == "invalid"
    assert "exited with 7" in result.metadata["review_contract_gap"]
