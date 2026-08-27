"""Controller lane artifact contract for graph cold-review execution."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

from runner.review_controller import (
    ExecutionReceipt,
    ReviewInputs,
    create_review_request,
    persist_controller_lane_artifacts,
)


def _request():
    return create_review_request(
        ReviewInputs(
            repository="example/repository",
            workspace_path="/workspace/repository",
            base_sha="a" * 40,
            head_sha="b" * 40,
            tree_sha="c" * 40,
            task_text="Review the change.",
        )
    )


@pytest.mark.parametrize(
    ("status", "verdict", "returncode"),
    (("valid", "pass", 0), ("valid", "fail", 0), ("invalid", "invalid", 7)),
)
def test_controller_lane_writes_authoritative_artifacts(
    tmp_path: Path, status: str, verdict: str, returncode: int
) -> None:
    request = _request()
    transport = '{"type":"agent_message","text":"review"}\n'
    response = "VERDICT: " + verdict + "\n"
    output_dir = tmp_path / "controller-reviews" / "run" / "cold_reviewer" / "1" / "primary"

    paths = persist_controller_lane_artifacts(
        request,
        output_dir=output_dir,
        lane="primary",
        backend="codex",
        neutral_cwd=output_dir.parent,
        transport_argv=("codex", "exec", "--json"),
        transport_text=transport,
        response_text=response,
        backend_returncode=returncode,
        status=status,
        verdict=verdict,
        receipts=(
            ExecutionReceipt(
                command="pytest -q",
                exit_code=0,
                output_sha256=hashlib.sha256(b"1 passed").hexdigest(),
            ),
        ),
    )

    required = {
        "prompt": "prompt.txt",
        "envelope": "envelope.json",
        "transport": "transport.jsonl",
        "response": "reviewer.output.md",
        "receipt": "controller-receipt.json",
    }
    assert set(paths) >= set(required)
    for key, filename in required.items():
        assert Path(paths[key]) == output_dir / filename
        assert (output_dir / filename).is_file()

    receipt = json.loads((output_dir / "controller-receipt.json").read_text())
    assert receipt["lane"] == "primary"
    assert receipt["status"] == status
    assert receipt["verdict"] == verdict
    assert receipt["backend_returncode"] == returncode
    assert receipt["prompt_sha256"] == hashlib.sha256(
        (output_dir / "prompt.txt").read_bytes()
    ).hexdigest()
    assert receipt["envelope_sha256"] == hashlib.sha256(
        (output_dir / "envelope.json").read_bytes()
    ).hexdigest()
    assert receipt["transport_sha256"] == hashlib.sha256(
        (output_dir / "transport.jsonl").read_bytes()
    ).hexdigest()
    assert receipt["response_sha256"] == hashlib.sha256(
        (output_dir / "reviewer.output.md").read_bytes()
    ).hexdigest()


def test_controller_lane_artifacts_are_separate_and_retry_safe(tmp_path: Path) -> None:
    request = _request()
    kwargs = {
        "request": request,
        "backend": "codex",
        "neutral_cwd": tmp_path / "controller-reviews" / "run" / "cold_reviewer",
        "transport_argv": ("codex", "exec", "--json"),
        "transport_text": "transport\n",
        "response_text": "response\n",
        "backend_returncode": 0,
        "status": "valid",
        "verdict": "pass",
        "receipts": (),
    }
    primary = persist_controller_lane_artifacts(
        output_dir=tmp_path / "controller-reviews" / "run" / "cold_reviewer" / "1" / "primary",
        lane="primary",
        **kwargs,
    )
    shadow = persist_controller_lane_artifacts(
        output_dir=tmp_path / "controller-reviews" / "run" / "cold_reviewer" / "1" / "shadow_codex",
        lane="shadow_codex",
        **kwargs,
    )
    assert primary["receipt"] != shadow["receipt"]
    assert Path(primary["receipt"]).parent != Path(shadow["receipt"]).parent
    with pytest.raises(FileExistsError):
        persist_controller_lane_artifacts(
            output_dir=Path(primary["receipt"]).parent,
            lane="primary",
            **kwargs,
        )
