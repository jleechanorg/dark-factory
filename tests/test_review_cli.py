"""Tests for the binary-owned ``dark-factory review`` entry point."""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import subprocess
from typing import ClassVar

import pytest

from runner.review_cli import _parser, _read_validated_task, main
from runner.handler_sandbox import _PinnedLauncherCommand
from runner.review_controller import ReviewContractError


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


def _task(repo):
    return repo / "value.txt"


def _valid_response(prompt: str, *, verdict: str = "pass") -> str:
    return json.dumps(
        {
            "verdict": verdict,
            "findings": [] if verdict == "pass" else ["blocking finding"],
            "evidence_checked": ["changed files and test output"],
            "commands_executed": ["python -m pytest -q"],
            "caveats": [],
        },
        separators=(",", ":"),
    )


def _valid_transport(prompt: str, *, verdict: str = "pass") -> str:
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
                        "text": _valid_response(prompt, verdict=verdict),
                    },
                }
            ),
        )
    )


def test_review_command_writes_valid_digest_bound_receipt(tmp_path, monkeypatch, capsys):
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
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
            "--evidence",
            "value.txt",
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


def test_review_cli_passes_and_closes_pinned_linux_launcher(tmp_path, monkeypatch, capsys):
    """The standalone CLI must inherit and then close a pinned launcher FD."""
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
    output = tmp_path / "review-output"
    real_run = subprocess.run
    seen: dict[str, object] = {}
    launcher_fd = os.open(os.devnull, os.O_RDONLY)
    command = _PinnedLauncherCommand(["codex", "exec", "-"], launcher_fd)

    class Runtime:
        run_dir = tmp_path / "runtime"
        codex_home = tmp_path / "codex-home"
        env: ClassVar[dict[str, str]] = {}

    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )
    monkeypatch.setattr("runner.review_cli._create_controller_runtime", lambda: Runtime())
    monkeypatch.setattr("runner.review_cli._controller_codex_args", lambda *args, **kwargs: command)
    monkeypatch.setattr("runner.review_cli._cleanup_controller_runtime", lambda path: None)

    def fake_run(process_args, **kwargs):
        if process_args[0] == "git":
            return real_run(process_args, **kwargs)
        seen["pass_fds"] = kwargs.get("pass_fds")
        return subprocess.CompletedProcess(
            process_args, 0, stdout=_valid_transport(kwargs["input"]), stderr=""
        )

    monkeypatch.setattr("runner.review_cli.subprocess.run", fake_run)
    rc = main(
        [
            "--workdir", str(repo),
            "--base-sha", base,
            "--head-sha", head,
            "--task-file", str(task),
            "--evidence", "value.txt",
            "--output-dir", str(output),
            "--backend", "codex",
        ]
    )

    assert rc == 0
    assert seen["pass_fds"] == (launcher_fd,)
    assert command.launcher_fd == -1
    assert json.loads(capsys.readouterr().out)["status"] == "valid"


def test_review_command_rejects_pass_without_evidence_manifest(
    tmp_path, monkeypatch, capsys
):
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
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
            command, 0, stdout=_valid_transport(kwargs["input"]), stderr=""
        )

    monkeypatch.setattr("runner.review_cli.subprocess.run", fake_run)
    rc = main(
        [
            "--workdir", str(repo),
            "--base-sha", base,
            "--head-sha", head,
            "--task-file", str(task),
            "--output-dir", str(output),
            "--backend", "codex",
        ]
    )

    assert rc == 1
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "invalid"
    assert "evidence manifest" in receipt["contract_error"]
    assert json.loads(capsys.readouterr().out)["status"] == "invalid"


def test_review_command_rejects_empty_success_receipt(
    tmp_path, monkeypatch, capsys
):
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
    output = tmp_path / "review-output"
    real_run = subprocess.run
    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )
    empty_transport = _valid_transport("").replace('"3 passed"', '""')

    def fake_run(command, **kwargs):
        if command[0] == "git":
            return real_run(command, **kwargs)
        return subprocess.CompletedProcess(command, 0, stdout=empty_transport, stderr="")

    monkeypatch.setattr("runner.review_cli.subprocess.run", fake_run)
    rc = main(
        [
            "--workdir", str(repo),
            "--base-sha", base,
            "--head-sha", head,
            "--task-file", str(task),
            "--evidence", "value.txt",
            "--output-dir", str(output),
            "--backend", "codex",
        ]
    )

    assert rc == 1
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "invalid"
    assert "non-empty output" in receipt["contract_error"]
    assert json.loads(capsys.readouterr().out)["status"] == "invalid"


def test_review_command_returns_two_for_valid_fail_verdict(
    tmp_path, monkeypatch, capsys
):
    """Transport validity must never be mistaken for review acceptance."""
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
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
            stdout=_valid_transport(kwargs["input"], verdict="fail"),
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

    assert rc == 2
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "valid"
    assert receipt["backend_returncode"] == 0
    assert receipt["verdict"] == "fail"
    assert json.loads(capsys.readouterr().out)["verdict"] == "fail"



@pytest.mark.parametrize(
    "stub_env",
    ("DARK_FACTORY_ITERATION_STUB", "DARK_FACTORY_FAKE_LLM"),
)
def test_review_command_rejects_pass_under_stub_env(
    tmp_path, monkeypatch, capsys, stub_env
):
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
    output = tmp_path / "review-output"
    real_run = subprocess.run
    monkeypatch.setenv(stub_env, "1")
    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )

    def fake_run(command, **kwargs):
        if command[0] == "git":
            return real_run(command, **kwargs)
        return subprocess.CompletedProcess(
            command, 0, stdout=_valid_transport(kwargs["input"]), stderr=""
        )

    monkeypatch.setattr("runner.review_cli.subprocess.run", fake_run)
    rc = main(
        [
            "--workdir", str(repo),
            "--base-sha", base,
            "--head-sha", head,
            "--task-file", str(task),
            "--evidence", "value.txt",
            "--output-dir", str(output),
            "--backend", "codex",
        ]
    )

    assert rc == 1
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "invalid"
    assert "stub-mode" in receipt["contract_error"]
    assert json.loads(capsys.readouterr().out)["status"] == "invalid"


def test_review_command_rejects_symlinked_parent_before_target_query(
    tmp_path, monkeypatch
):
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
    alias_parent = tmp_path / "alias-parent"
    alias_parent.symlink_to(tmp_path, target_is_directory=True)
    target_operation_attempted = False

    def unexpected_target_operation(*args, **kwargs):
        nonlocal target_operation_attempted
        target_operation_attempted = True
        raise AssertionError("target must not be queried through a symlink")

    monkeypatch.setattr("runner.review_cli.subprocess.run", unexpected_target_operation)
    rc = main(
        [
            "--workdir", str(alias_parent / repo.name),
            "--base-sha", base,
            "--head-sha", head,
            "--task-file", str(task),
            "--output-dir", str(tmp_path / "review-output"),
        ]
    )

    assert rc == 1
    assert target_operation_attempted is False


@pytest.mark.parametrize(
    "discovery_error", (OSError("unavailable"), RuntimeError("missing"))
)
def test_review_command_fails_closed_when_holdout_discovery_fails(
    tmp_path, monkeypatch, discovery_error
):
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
    launched = False

    def unavailable():
        raise type(discovery_error)(str(discovery_error))

    def unexpected_launch(*args, **kwargs):
        nonlocal launched
        launched = True
        raise AssertionError("holdout discovery must fail before reviewer launch")

    monkeypatch.setattr(
        "runner.handler_sandbox._holdout_denied_paths", unavailable
    )
    monkeypatch.setattr("runner.review_cli._gate_subprocess_args", unexpected_launch)
    monkeypatch.setattr(
        "runner.review_cli._snapshot",
        lambda *args, **kwargs: pytest.fail(
            "holdout discovery must fail before snapshot"
        ),
    )

    rc = main(
        [
            "--workdir", str(repo),
            "--base-sha", base,
            "--head-sha", head,
            "--task-file", str(task),
            "--evidence", "value.txt",
            "--output-dir", str(tmp_path / "review-output"),
        ]
    )

    assert rc == 1
    assert launched is False


def test_task_file_rejects_external_holdout_and_symlink_before_backend(tmp_path, monkeypatch):
    repo, _base, _head = _repo(tmp_path)
    outside = tmp_path / "outside.md"
    outside.write_text("outside", encoding="utf-8")
    holdout = tmp_path / "holdout"
    holdout.mkdir()
    held = holdout / "task.md"
    held.write_text("sealed", encoding="utf-8")
    linked = repo / "task-link.md"
    linked.symlink_to(outside)

    for candidate in (outside, held, linked):
        with pytest.raises(ReviewContractError):
            _read_validated_task(candidate, repo, (str(holdout),))

    launched = False

    def unexpected_launch(*args, **kwargs):
        nonlocal launched
        launched = True
        raise AssertionError("invalid task file must fail before backend")

    monkeypatch.setattr("runner.review_cli._gate_subprocess_args", unexpected_launch)
    rc = main(
        [
            "--workdir", str(repo),
            "--base-sha", _base,
            "--head-sha", _head,
            "--task-file", str(outside),
            "--evidence", "value.txt",
            "--output-dir", str(tmp_path / "review-output"),
        ]
    )
    assert rc == 1
    assert launched is False


@pytest.mark.parametrize("replacement", ("symlink", "hardlink"))
def test_task_file_read_binds_descriptor_before_replacement(tmp_path, replacement, monkeypatch):
    repo, _base, _head = _repo(tmp_path)
    task = repo / "task.md"
    task.write_text("review task\n", encoding="utf-8")
    holdout = tmp_path / "holdout"
    holdout.mkdir()
    sealed = holdout / "sealed.md"
    sealed.write_text("sealed\n", encoding="utf-8")
    real_open = os.open

    def race_open(path, flags, *args):
        if pathlib.Path(path) == task:
            task.unlink()
            if replacement == "symlink":
                task.symlink_to(sealed)
            else:
                os.link(sealed, task)
        return real_open(path, flags, *args)

    monkeypatch.setattr("runner.review_cli.os.open", race_open)
    with pytest.raises(ReviewContractError, match="safe|regular"):
        _read_validated_task(task, repo, (str(holdout),))


def test_review_command_rejects_post_request_symlink_swap(tmp_path, monkeypatch):
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
    output = tmp_path / "review-output"
    real_run = subprocess.run
    swapped = False
    monkeypatch.setattr(
        "runner.review_cli._gate_subprocess_args",
        lambda backend, prompt, ctx, timeout: ["codex", "exec", prompt],
    )

    def fake_run(command, **kwargs):
        nonlocal swapped
        if command[0] == "git":
            return real_run(command, **kwargs)
        moved = tmp_path / "reviewed-repo-moved"
        repo.rename(moved)
        repo.symlink_to(moved, target_is_directory=True)
        swapped = True
        return subprocess.CompletedProcess(
            command, 0, stdout=_valid_transport(kwargs["input"]), stderr=""
        )

    monkeypatch.setattr("runner.review_cli.subprocess.run", fake_run)
    rc = main(
        [
            "--workdir", str(repo),
            "--base-sha", base,
            "--head-sha", head,
            "--task-file", str(task),
            "--output-dir", str(output),
            "--backend", "codex",
        ]
    )

    assert swapped is True
    assert rc == 1
    receipt = json.loads((output / "controller-receipt.json").read_text())
    assert receipt["status"] == "invalid"
    assert "symlink" in receipt["contract_error"]


def test_review_command_fails_closed_on_unstructured_response(
    tmp_path, monkeypatch
):
    repo, base, head = _repo(tmp_path)
    task = _task(repo)
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
    task = _task(repo)
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


@pytest.mark.parametrize("backend", ("claude", "agy", "minimax", "claude-sonnet"))
def test_review_cli_rejects_backends_without_captured_command_receipts(backend):
    """Standalone controller review accepts only its JSONL Codex transport."""
    backend_action = next(
        action for action in _parser()._actions if action.dest == "backend"
    )

    assert backend not in backend_action.choices


def test_review_wrapper_does_not_rewrite_codex_to_non_codex_fallback(tmp_path):
    """A review request must reach the Codex-only parser unchanged."""
    root = tmp_path / "dark-factory"
    wrapper = root / "bin" / "dark-factory"
    python = root / ".venv" / "bin" / "python"
    wrapper.parent.mkdir(parents=True)
    python.parent.mkdir(parents=True)
    wrapper.write_text(
        pathlib.Path(__file__).parents[1].joinpath("bin/dark-factory").read_text(),
        encoding="utf-8",
    )
    wrapper.chmod(0o755)
    python.write_text(
        "#!/bin/sh\n"
        "if [ \"$2\" = runner.preflight ]; then\n"
        "  printf '%s\\n' '{\"status\":\"warn\",\"fallback_recommendation\":\"claude\"}'\n"
        "  exit 0\n"
        "fi\n"
        "printf '%s\\n' \"$*\"\n",
        encoding="utf-8",
    )
    python.chmod(0o755)

    completed = subprocess.run(
        [str(wrapper), "review", "--backend", "codex"],
        cwd=tmp_path,
        env={"PATH": os.environ["PATH"]},
        capture_output=True,
        text=True,
        check=False,
    )

    assert completed.returncode == 0
    assert "review --backend codex" in completed.stdout
    assert "review --backend claude" not in completed.stdout
