"""Controller lane artifact contract for graph cold-review execution."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

from runner.handler_core import Result
from runner import handler_dispatch
from runner.handler_parallel_reviewer import _persist_controller_lane
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
    assert {
        "contract_error",
        "base_sha",
        "tree_sha",
        "lane",
        "cleanup_policy",
        "status",
        "verdict",
        "returncode",
        "prompt_sha256",
        "envelope_sha256",
        "transport_sha256",
        "response_sha256",
    } <= set(receipt)
    assert receipt["lane"] == "primary"
    assert receipt["status"] == status
    assert receipt["verdict"] == verdict
    assert receipt["backend_returncode"] == returncode
    assert receipt["returncode"] == returncode
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


def test_graph_lane_preserves_integer_zero_transport_exit_code(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The graph persistence seam must record a successful integer zero."""
    request = _request()
    captured: dict[str, object] = {}

    def fake_persist(_request_arg, **kwargs):
        captured.update(kwargs)
        return {"receipt": str(tmp_path / "controller-receipt.json")}

    monkeypatch.setattr(
        "runner.review_controller.persist_controller_lane_artifacts", fake_persist
    )
    result = _persist_controller_lane(
        Result(
            outcome="success",
            output='{"verdict":"pass"}',
            metadata={"returncode": 0, "review_contract_status": "valid", "verdict": "pass"},
        ),
        request,
        lane="primary",
        output_dir=tmp_path / "primary",
        neutral_cwd=tmp_path,
    )

    assert captured["backend_returncode"] == 0
    assert result.metadata["controller_primary_artifact_status"] == "persisted"


def test_shadow_backend_override_keeps_allocated_controller_lane(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An env backend override cannot redirect artifacts to a missing lane."""
    request = _request()
    neutral = tmp_path / "neutral"
    neutral.mkdir()
    monkeypatch.setenv("DARK_FACTORY_SHADOW_BACKEND", "claude")
    monkeypatch.setattr(handler_dispatch.shutil, "which", lambda _name: "/usr/bin/claude")
    monkeypatch.setattr(
        handler_dispatch,
        "_gate_subprocess_args",
        lambda *_args, **_kwargs: ["/usr/bin/claude", "--print", "-"],
    )
    ctx = type("ContextStub", (), {"state": {
        "_df_controller_review_lane_dirs": json.dumps(
            {"shadow_codex": str(neutral / "shadow_codex")}
        ),
    }})()

    shadow = handler_dispatch._launch_shadow_gate_review(
        "cold_reviewer",
        request.prompt,
        request.head_sha,
        30,
        ctx,
        backend="codex",
        prompt_is_complete=True,
        read_only_path=tmp_path,
        lane_name="shadow_codex",
    )

    assert shadow is not None
    assert shadow.backend == "claude"
    assert shadow.lane_name == "shadow_codex"
    assert shadow.output_dir == neutral / "shadow_codex"
    assert "codex backend" in shadow.launch_error

    persisted = _persist_controller_lane(
        Result(
            outcome="failure",
            output="controller launch failed",
            metadata={
                "returncode": -1,
                "review_contract_status": "invalid",
                "review_contract_gap": shadow.launch_error,
                "verdict": "fail",
                "reviewer_backend": shadow.backend,
            },
        ),
        request,
        lane=shadow.lane_name,
        output_dir=shadow.output_dir,
        neutral_cwd=neutral,
    )
    receipt = json.loads(Path(persisted.metadata["controller_shadow_codex_receipt_path"]).read_text())
    assert receipt["lane"] == "shadow_codex"
    assert receipt["backend"] == "claude"
    assert receipt["status"] == "invalid"
    assert receipt["contract_error"]
