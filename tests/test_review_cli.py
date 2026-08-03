"""Tests for the binary-owned ``dark-factory review`` entry point."""

from __future__ import annotations

import json
import hashlib
import pathlib
import re
import subprocess
from unittest.mock import patch

import pytest

from runner.review_cli import main
from runner.review_controller import CHECK_IDS


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
    assert receipt["fallback_used"] is False
    assert receipt["backend"] == "codex"
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


def test_review_command_delegates_to_canonical_executor_once(
    tmp_path, monkeypatch, capsys
):
    """The CLI freezes inputs, then uses the shared controller executor once."""
    from runner.review_controller import run_controller_review as canonical_run

    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    output = tmp_path / "canonical-output"
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
    calls = []

    def canonical_spy(request, **kwargs):
        calls.append((request, kwargs))
        return canonical_run(request, **kwargs)

    with patch("runner.review_cli.run_controller_review", canonical_spy):
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
    assert len(calls) == 1
    assert calls[0][1]["output_dir"] == output.resolve()
    assert json.loads(capsys.readouterr().out)["verdict"] == "pass"


def test_review_command_accepts_only_canonical_codex_backend():
    from runner.review_cli import _parser

    with pytest.raises(SystemExit):
        _parser().parse_args(
            [
                "--base-sha",
                "a" * 40,
                "--head-sha",
                "b" * 40,
                "--task-file",
                "task.md",
                "--output-dir",
                "out",
                "--backend",
                "claude",
            ]
        )


def test_review_command_never_overwrites_unclaimed_output_receipt(
    tmp_path, monkeypatch
):
    """A failed output-directory ownership claim preserves every existing byte."""
    repo, base, head = _repo(tmp_path)
    task = tmp_path / "task.md"
    task.write_text("Review the behavior change.", encoding="utf-8")
    output = tmp_path / "already-owned"
    output.mkdir()
    receipt = output / "controller-receipt.json"
    sentinel = b'{"owner":"another-run","status":"valid"}\n'
    receipt.write_bytes(sentinel)

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
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
            "codex",
        ]
    )

    assert rc == 1
    assert receipt.read_bytes() == sentinel
    assert sorted(path.name for path in output.iterdir()) == [
        "controller-receipt.json"
    ]


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
    assert receipt["fallback_used"] is False
    assert receipt["backend"] == "codex"
    assert receipt["base_sha"] == base
    assert "invalid JSONL" in receipt["contract_error"]


def test_review_skills_describe_one_canonical_codex_controller_lane():
    root = pathlib.Path(__file__).resolve().parents[1]
    dark_factory = (root / ".claude/skills/dark-factory/SKILL.md").read_text()
    calibration = (
        root / ".claude/skills/reviewer-calibration/SKILL.md"
    ).read_text()

    for text in (dark_factory, calibration):
        assert "canonical Codex-only" in text
        assert "--backend <backend>" not in text
        assert "each available reviewer backend" not in text
    assert "ordinary graph shadow" in dark_factory
    assert "ordinary graph shadow" in calibration
    assert "VERDICT: pass|fail" in calibration
    assert "transport.jsonl" in calibration
    assert '"verdict": "blockers|no_blockers|inconclusive"' not in calibration


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
