"""Tests for the binary-owned ``dark-factory review`` entry point."""

from __future__ import annotations

import json
import hashlib
import re
import subprocess
from types import SimpleNamespace

import pytest

from runner.review_cli import main
from runner.review_controller import CHECK_IDS, ReviewContractError


def _repo(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.email", "review@example.invalid"], cwd=repo, check=True)
    subprocess.run(["git", "config", "user.name", "Review Test"], cwd=repo, check=True)
    (repo / "value.txt").write_text("before\n", encoding="utf-8")
    subprocess.run(["git", "add", "value.txt"], cwd=repo, check=True)
    subprocess.run(["git", "commit", "-qm", "base"], cwd=repo, check=True)
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, check=True, capture_output=True, text=True
    ).stdout.strip()
    (repo / "value.txt").write_text("after\n", encoding="utf-8")
    subprocess.run(["git", "commit", "-qam", "head"], cwd=repo, check=True)
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, check=True, capture_output=True, text=True
    ).stdout.strip()
    return repo, base, head


def _valid_response(prompt: str) -> str:
    keys = (
        "PROMPT_ID",
        "PROMPT_SHA256",
        "ENVELOPE_SHA256",
        "HEAD_SHA",
        "TASK_SHA256",
        "DIFF_SHA256",
        "CHANGED_FILES_SHA256",
        "EVIDENCE_MANIFEST_SHA256",
    )
    values = {}
    for key in keys:
        match = re.search(rf"^{key}: (\S+)$", prompt, re.MULTILINE)
        assert match, f"prompt is missing required binding line: {key}"
        values[key] = match.group(1)
    return "\n".join(
        [
            *(f"{key}: {values[key]}" for key in keys),
            "VERDICT: pass",
            *(f"{check_id}: pass" for check_id in CHECK_IDS),
            "",
            "## Findings",
            "None; inspected the changed implementation and callers.",
            "## Commands Executed",
            "`python -m pytest` — exit code 0.",
            "## Evidence Checked",
            "Changed files and test output.",
            "## Caveats",
            "None.",
        ]
    )


def _valid_transport(prompt: str) -> str:
    return "\n".join(
        (
            json.dumps(
                {
                    "type": "item.completed",
                    "item": {
                        "type": "command_execution",
                        "command": "python -m pytest -q",
                        "exit_code": 0,
                        "aggregated_output": "3 passed",
                    },
                }
            ),
            json.dumps(
                {
                    "type": "item.completed",
                    "item": {
                        "type": "agent_message",
                        "text": _valid_response(prompt),
                    },
                }
            ),
        )
    )


def test_review_command_writes_valid_digest_bound_receipt(tmp_path, monkeypatch, capsys):
    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    output = tmp_path / "review-output"
    real_run = subprocess.run

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )

    def fake_run(command, **kwargs):
        if command[0] == "git":
            return real_run(command, **kwargs)
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=_valid_transport(kwargs["input"]),
            stderr="",
        )

    monkeypatch.setattr("runner.review_cli.subprocess.run", fake_run)

    rc = main(
        [
            "--workdir",
            str(repo),
            "--base-sha",
            base,
            "--head-sha",
            head,
            "--task-file",
            str(task),
            "--output-dir",
            str(output),
            "--backend",
            "codex",
        ]
    )

    assert rc == 0
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "valid"
    assert receipt["verdict"] == "pass"
    assert receipt["review_contract"] == "cold-review-v1"
    assert receipt["head_sha"] == head
    assert len(receipt["prompt_sha256"]) == 64
    assert len(receipt["prompt_payload_sha256"]) == 64
    assert receipt["prompt_path"] == "prompt.txt"
    assert receipt["envelope_path"] == "envelope.json"
    assert receipt["response_path"] == "reviewer.output.md"
    assert receipt["transport_path"] == "transport.jsonl"
    assert receipt["command_receipts"][0]["exit_code"] == 0
    assert receipt["envelope_sha256"] == hashlib.sha256(
        (output / "envelope.json").read_bytes()
    ).hexdigest()
    assert receipt["response_sha256"] == hashlib.sha256(
        (output / "reviewer.output.md").read_bytes()
    ).hexdigest()
    assert (output / "prompt.txt").is_file()
    assert (output / "envelope.json").is_file()
    assert (output / "reviewer.output.md").is_file()
    assert json.loads(capsys.readouterr().out)["status"] == "valid"


def test_review_command_routes_explicit_v2_contract_to_shared_controller(
    tmp_path, monkeypatch
):
    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    captured = []

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )

    def fake_run_controller_review(request, **kwargs):
        captured.append((request, kwargs))
        output_dir = kwargs["output_dir"]
        output_dir.mkdir()
        (output_dir / "controller-receipt.json").write_text(
            json.dumps({"exit_code": 0}), encoding="utf-8"
        )
        return SimpleNamespace(
            review=SimpleNamespace(verdict="pass", response_sha256="a" * 64),
            receipts=(),
            transport_text="",
            output_paths={},
        )

    monkeypatch.setattr(
        "runner.review_cli.run_controller_review", fake_run_controller_review
    )

    rc = main(
        [
            "--workdir",
            str(repo),
            "--base-sha",
            base,
            "--head-sha",
            head,
            "--task-file",
            str(task),
            "--output-dir",
            str(tmp_path / "review-output"),
            "--review-contract",
            "cold-review-v2",
        ]
    )

    assert rc == 0
    assert len(captured) == 1
    request, kwargs = captured[0]
    assert request.review_contract == "cold-review-v2"
    assert request.prompt_id == "controller-cold-review-v2"
    assert kwargs["neutral_cwd"] == tmp_path / "review-output"
    assert kwargs["transport_argv"][:3] == ("codex", "exec", "--json")
    assert kwargs["transport_is_jsonl"] is True


def test_review_command_preserves_non_codex_backend_and_sanitized_env(
    tmp_path, monkeypatch
):
    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    output = tmp_path / "review-output"
    captured = []
    transport_env = {"ANTHROPIC_BASE_URL": "https://api.minimax.io/anthropic"}

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["claude", "--print", prompt],
    )
    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_env",
        lambda backend: transport_env,
    )

    def fake_run_controller_review(request, **kwargs):
        captured.append((request, kwargs))
        output_dir = kwargs["output_dir"]
        output_dir.mkdir()
        (output_dir / "controller-receipt.json").write_text(
            json.dumps({"exit_code": 0}), encoding="utf-8"
        )
        return SimpleNamespace(
            review=SimpleNamespace(verdict="pass", response_sha256="a" * 64),
            receipts=(),
            transport_text="",
            output_paths={},
        )

    monkeypatch.setattr(
        "runner.review_cli.run_controller_review", fake_run_controller_review
    )

    rc = main(
        [
            "--workdir",
            str(repo),
            "--base-sha",
            base,
            "--head-sha",
            head,
            "--task-file",
            str(task),
            "--output-dir",
            str(output),
            "--backend",
            "minimax",
        ]
    )

    assert rc == 0
    assert len(captured) == 1
    _, kwargs = captured[0]
    assert kwargs["transport_argv"] == ("claude", "--print", captured[0][0].prompt)
    assert kwargs["transport_env"] is transport_env
    assert kwargs["transport_is_jsonl"] is False


def test_review_command_preserves_shared_error_backend_returncode(
    tmp_path, monkeypatch
):
    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    output = tmp_path / "review-output"

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )

    def fake_run_controller_review(request, **kwargs):
        output_dir = kwargs["output_dir"]
        output_dir.mkdir()
        (output_dir / "controller-receipt.json").write_text(
            json.dumps({"exit_code": 17}), encoding="utf-8"
        )
        raise ReviewContractError("review backend exited with 17")

    monkeypatch.setattr(
        "runner.review_cli.run_controller_review", fake_run_controller_review
    )

    rc = main(
        [
            "--workdir",
            str(repo),
            "--base-sha",
            base,
            "--head-sha",
            head,
            "--task-file",
            str(task),
            "--output-dir",
            str(output),
        ]
    )

    assert rc == 1
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "invalid"
    assert receipt["backend_returncode"] == 17


def test_review_command_treats_nonzero_shared_backend_exit_as_invalid(
    tmp_path, monkeypatch
):
    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    output = tmp_path / "review-output"

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )

    def fake_run_controller_review(request, **kwargs):
        output_dir = kwargs["output_dir"]
        output_dir.mkdir()
        (output_dir / "controller-receipt.json").write_text(
            json.dumps({"exit_code": 9}), encoding="utf-8"
        )
        return SimpleNamespace(
            review=SimpleNamespace(verdict="pass", response_sha256="a" * 64),
            receipts=(),
            transport_text="",
            output_paths={},
        )

    monkeypatch.setattr(
        "runner.review_cli.run_controller_review", fake_run_controller_review
    )

    rc = main(
        [
            "--workdir",
            str(repo),
            "--base-sha",
            base,
            "--head-sha",
            head,
            "--task-file",
            str(task),
            "--output-dir",
            str(output),
        ]
    )

    assert rc == 1
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "invalid"
    assert receipt["verdict"] == "invalid"
    assert receipt["contract_error"] == "review backend exited with 9"


def test_review_command_fails_closed_on_unstructured_response(
    tmp_path, monkeypatch
):
    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    output = tmp_path / "review-output"
    real_run = subprocess.run

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )

    def fake_run(command, **kwargs):
        if command[0] == "git":
            return real_run(command, **kwargs)
        return subprocess.CompletedProcess(command, 0, stdout="looks good", stderr="")

    monkeypatch.setattr("runner.review_cli.subprocess.run", fake_run)

    rc = main(
        [
            "--workdir",
            str(repo),
            "--base-sha",
            base,
            "--head-sha",
            head,
            "--task-file",
            str(task),
            "--output-dir",
            str(output),
        ]
    )

    assert rc == 1
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "invalid"
    assert "invalid JSONL" in receipt["contract_error"]


def test_review_command_rejects_dirty_workspace_before_backend(
    tmp_path, monkeypatch
):
    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    (repo / "untracked.txt").write_text("not frozen\n", encoding="utf-8")
    launched = False

    def unexpected_launch(*args, **kwargs):
        nonlocal launched
        launched = True
        return ["reviewer"]

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        unexpected_launch,
    )

    rc = main(
        [
            "--workdir",
            str(repo),
            "--base-sha",
            base,
            "--head-sha",
            head,
            "--task-file",
            str(task),
            "--output-dir",
            str(tmp_path / "review-output"),
        ]
    )

    assert rc == 1
    assert launched is False


def test_main_entrypoint_dispatches_review_subcommand(capsys):
    """runner.__main__.main(["review", "--help"]) dispatches to review_cli.main."""
    from runner.__main__ import main as runner_main

    with pytest.raises(SystemExit) as exc_info:
        runner_main(["review", "--help"])

    assert exc_info.value.code == 0
    captured = capsys.readouterr()
    assert "--base-sha" in captured.out
    assert "--head-sha" in captured.out
    assert "--task-file" in captured.out
    assert "--output-dir" in captured.out
    assert "--backend" in captured.out
    assert "--review-contract" in captured.out
