"""Regression tests for controller transport isolation and exit status."""

from __future__ import annotations

import hashlib
import json
import pathlib
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile

import pytest

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


def test_controller_transport_uses_one_macos_sandbox_for_read_only_review(
    tmp_path,
):
    """macOS must combine holdout denial + read-only in one outer sandbox."""
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

    transport = _build_controller_codex_transport(
        sandboxed, read_only_path=tmp_path
    )

    assert transport[:2] == sandboxed[:2]
    assert transport[3:] == [
        "/usr/local/bin/codex",
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "--dangerously-bypass-approvals-and-sandbox",
        "-",
    ]
    assert '(deny file-read* (subpath "/sealed/holdouts"))' in transport[2]
    assert f'(deny file-write* (subpath "{tmp_path}"))' in transport[2]
    assert "--sandbox" not in transport


def test_controller_transport_macos_profile_enforces_read_only_and_holdout_denial(
    tmp_path,
):
    """A constructed macOS transport launches and enforces both boundaries."""
    if sys.platform != "darwin" or shutil.which("sandbox-exec") is None:
        pytest.skip("macOS sandbox-exec unavailable")
    holdout = (
        pathlib.Path.home()
        / "projects"
        / "dark-factory-holdouts"
        / "holdouts"
        / "hello"
        / "scenarios.yaml"
    )
    if not holdout.is_file():
        pytest.skip(f"real holdout target missing: {holdout}")

    cache_root = pathlib.Path.home() / "Library" / "Caches"
    cache_root.mkdir(parents=True, exist_ok=True)
    target_root = pathlib.Path(tempfile.mkdtemp(prefix="df-pr771-", dir=cache_root))
    try:
        allowed = target_root / "allowed.txt"
        target = target_root / "target.txt"
        allowed.write_text("allowed\n", encoding="utf-8")
        target.write_text("unchanged\n", encoding="utf-8")
        codex = tmp_path / "codex"
        codex.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            f"allowed=$(cat {shlex.quote(str(allowed))})\n"
            f"if cat {shlex.quote(str(holdout))} >/dev/null 2>&1; then\n"
            "  echo holdout-readable >&2\n"
            "  exit 11\n"
            "fi\n"
            f"if printf changed > {shlex.quote(str(target))}; then\n"
            "  echo target-write-allowed >&2\n"
            "  exit 12\n"
            "fi\n"
            "printf '{\"allowed\":\"%s\",\"holdout_denied\":true,"
            "\"write_denied\":true}\n' \"$allowed\"\n",
            encoding="utf-8",
        )
        codex.chmod(codex.stat().st_mode | stat.S_IXUSR)
        sandboxed = [
            str(pathlib.Path(shutil.which("sandbox-exec") or "sandbox-exec")),
            "-p",
            '(version 1)\n(allow default)\n'
            f'(deny file-read* (subpath "{holdout}"))\n',
            str(codex),
            "exec",
            "--yolo",
            "--skip-git-repo-check",
            "ignored prompt",
        ]

        transport = _build_controller_codex_transport(
            sandboxed, read_only_path=target_root
        )
        proc = subprocess.run(
            transport,
            input="{}\n",
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )

        assert proc.returncode == 0, proc.stderr
        assert '"allowed":"allowed"' in proc.stdout
        assert '"holdout_denied":true' in proc.stdout
        assert '"write_denied":true' in proc.stdout
        assert target.read_text(encoding="utf-8") == "unchanged\n"
    finally:
        shutil.rmtree(target_root)


def test_controller_transport_keeps_linux_deny_paths_and_native_read_only(
    monkeypatch, tmp_path
):
    """Linux keeps the preload deny prefix and Codex native read-only mode."""
    monkeypatch.setattr("runner.handler_dispatch.sys.platform", "linux")
    sandboxed = [
        "/usr/bin/env",
        "LD_PRELOAD=/tmp/deny_paths.so",
        "DENY_PATHS=/sealed/holdouts",
        "/usr/local/bin/codex",
        "exec",
        "--yolo",
        "--skip-git-repo-check",
        "ignored prompt",
    ]

    transport = _build_controller_codex_transport(
        sandboxed, read_only_path=tmp_path
    )

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
