"""Regression tests for controller transport isolation and exit status."""

from __future__ import annotations

import hashlib
import json
import pathlib
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


darwin_only = pytest.mark.skipif(
    sys.platform != "darwin", reason="Seatbelt is macOS-only"
)


@darwin_only
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
        "--ignore-user-config",
        "--skip-git-repo-check",
        "--disable",
        "shell_tool",
        "--disable",
        "unified_exec",
        "--disable",
        "browser_use",
        "--disable",
        "computer_use",
        "--config",
        'web_search="disabled"',
        "--ignore-rules",
        "-",
    ]
    assert '(deny file-read* (subpath "/sealed/holdouts"))' in transport[2]
    assert "(deny file-write*)" in transport[2]
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
    try:
        holdout_exists = holdout.is_file()
    except OSError as exc:
        pytest.skip(f"cannot probe real holdout target: {exc}")
    if not holdout_exists:
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
            "#!/usr/bin/env python3\n"
            "import errno\n"
            "import json\n"
            "from pathlib import Path\n"
            f"allowed = Path({str(allowed)!r}).read_text(encoding='utf-8')\n"
            f"holdout = Path({str(holdout)!r})\n"
            "try:\n"
            "    holdout.read_bytes()\n"
            "except OSError as exc:\n"
            "    if exc.errno not in (errno.EPERM, errno.EACCES):\n"
            "        raise SystemExit(f'holdout probe failed: {exc}')\n"
            "    holdout_denied = True\n"
            "else:\n"
            "    raise SystemExit('holdout-readable')\n"
            f"target = Path({str(target)!r})\n"
            "try:\n"
            "    target.write_text('changed', encoding='utf-8')\n"
            "except OSError as exc:\n"
            "    if exc.errno not in (errno.EPERM, errno.EACCES):\n"
            "        raise SystemExit(f'write probe failed: {exc}')\n"
            "    write_denied = True\n"
            "else:\n"
            "    raise SystemExit('target-write-allowed')\n"
            "print(json.dumps({'allowed': allowed.strip(), "
            "'holdout_denied': holdout_denied, 'write_denied': write_denied}))\n",
            encoding="utf-8",
        )
        codex.chmod(codex.stat().st_mode | stat.S_IXUSR)
        sandboxed = [
            str(pathlib.Path(shutil.which("sandbox-exec") or "sandbox-exec")),
            "-p",
            (
                '(version 1)\n(allow default)\n'
                f'(deny file-read* (subpath "{holdout}"))\n'
            ),
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
        assert json.loads(proc.stdout) == {
            "allowed": "allowed",
            "holdout_denied": True,
            "write_denied": True,
        }
        assert target.read_text(encoding="utf-8") == "unchanged\n"
    finally:
        shutil.rmtree(target_root)


def test_controller_transport_keeps_linux_deny_paths_and_native_read_only(
    monkeypatch, tmp_path
):
    """Linux adds kernel Landlock while retaining preload defense-in-depth."""
    monkeypatch.setattr("runner.handler_dispatch.sys.platform", "linux")
    monkeypatch.setattr(
        "runner.handlers._linux_controller_sandbox_prefix",
        lambda **kwargs: [
            "/usr/bin/env",
            "LD_PRELOAD=/tmp/deny_paths.so",
            "DENY_PATHS=/sealed/holdouts",
            "/opt/dark-factory/landlock-launcher",
            "--read",
            str(tmp_path),
            "--",
        ],
    )
    monkeypatch.setattr("runner.handlers._linux_codex_runtime_paths", lambda path: [])
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

    assert transport[:7] == [
        "/usr/bin/env",
        "LD_PRELOAD=/tmp/deny_paths.so",
        "DENY_PATHS=/sealed/holdouts",
        "/opt/dark-factory/landlock-launcher",
        "--read",
        str(tmp_path),
        "--",
    ]
    assert transport[7:] == [
        "/usr/local/bin/codex",
        "exec",
        "--json",
        "--ephemeral",
        "--ignore-user-config",
        "--skip-git-repo-check",
        "--disable",
        "shell_tool",
        "--disable",
        "unified_exec",
        "--disable",
        "browser_use",
        "--disable",
        "computer_use",
        "--config",
        'web_search="disabled"',
        "--ignore-rules",
        "-",
    ]
    assert "features.use_legacy_landlock=true" not in transport


def test_controller_transport_binds_controller_owned_schema(monkeypatch, tmp_path):
    monkeypatch.setattr("runner.handler_dispatch.sys.platform", "linux")
    monkeypatch.setattr(
        "runner.handlers._linux_controller_sandbox_prefix",
        lambda **kwargs: ["/opt/landlock", "DENY_PATHS=/sealed", "--"],
    )
    monkeypatch.setattr("runner.handlers._linux_codex_runtime_paths", lambda path: [])
    schema = tmp_path / "schema.json"
    schema.write_text("{}", encoding="utf-8")
    transport = _build_controller_codex_transport(
        [
            "/usr/bin/env", "DENY_PATHS=/sealed", "/usr/local/bin/codex", "exec",
            "--skip-git-repo-check", "ignored",
        ],
        read_only_path=tmp_path,
        schema_path=schema,
    )
    assert transport[-3:] == ["--output-schema", str(schema), "-"]
    assert "--sandbox" not in transport
    assert "features.use_legacy_landlock=true" not in transport


@pytest.mark.parametrize("platform", ["darwin", "linux"])
def test_controller_transport_rejects_disable_sandbox(
    monkeypatch, tmp_path, platform
):
    """Controller reviews must not use the testing sandbox escape hatch."""
    monkeypatch.setattr("runner.handler_dispatch.sys.platform", platform)
    monkeypatch.setenv("DISABLE_SANDBOX", "1")

    with pytest.raises(ValueError, match="DISABLE_SANDBOX"):
        _build_controller_codex_transport(
            [
                "codex",
                "exec",
                "--yolo",
                "--skip-git-repo-check",
                "ignored prompt",
            ],
            read_only_path=tmp_path,
        )


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
