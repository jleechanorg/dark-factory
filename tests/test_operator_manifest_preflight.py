"""Preflight + split-path resolution for operator_verify manifests."""

from __future__ import annotations

import json
import os
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))


def _init_git_repo(path: pathlib.Path) -> None:
    subprocess.run(["/usr/bin/git", "init", "-q"], cwd=path, check=True)
    subprocess.run(
        [
            "/usr/bin/git",
            "config",
            "user.email",
            "jleechan2015@users.noreply.github.com",
        ],
        cwd=path,
        check=True,
    )
    subprocess.run(
        ["/usr/bin/git", "config", "user.name", "Test"],
        cwd=path,
        check=True,
    )


def _repo_head(path: pathlib.Path) -> str:
    return subprocess.run(
        ["/usr/bin/git", "rev-parse", "HEAD"],
        cwd=path,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def _write_manifest(path: pathlib.Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body, encoding="utf-8")


_MANIFEST_BODY = """operator_verification:
  schema_version: 1
  commands:
    - id: pinned
      argv: ["@runner-python", "-m", "pytest", "-q"]
      lane: worker_safe
      timeout_seconds: 30
      classification: required
  exclusions: []
"""


def test_canonical_manifest_path_preferred_over_legacy_at_trust_head(
    tmp_path: pathlib.Path,
) -> None:
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha

    _init_git_repo(tmp_path)
    _write_manifest(tmp_path / ".dark-factory" / "evidence.yaml", _MANIFEST_BODY)
    _write_manifest(
        tmp_path / ".github" / "dark-factory-operator.yaml",
        _MANIFEST_BODY.replace("pinned", "canonical"),
    )
    subprocess.run(["/usr/bin/git", "add", "."], cwd=tmp_path, check=True)
    subprocess.run(
        ["/usr/bin/git", "commit", "-qm", "manifests"], cwd=tmp_path, check=True
    )
    head = subprocess.run(
        ["/usr/bin/git", "rev-parse", "HEAD"],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()

    manifest = ha._load_operator_manifest(tmp_path, trusted_head=head)

    assert manifest.commands[0].id == "canonical"
    assert manifest.path.as_posix().endswith(".github/dark-factory-operator.yaml")


def test_legacy_manifest_still_loads_from_git_history(tmp_path: pathlib.Path) -> None:
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha

    _init_git_repo(tmp_path)
    _write_manifest(tmp_path / ".dark-factory" / "evidence.yaml", _MANIFEST_BODY)
    subprocess.run(
        ["/usr/bin/git", "add", ".dark-factory/evidence.yaml"],
        cwd=tmp_path,
        check=True,
    )
    subprocess.run(
        ["/usr/bin/git", "commit", "-qm", "legacy"], cwd=tmp_path, check=True
    )
    head = subprocess.run(
        ["/usr/bin/git", "rev-parse", "HEAD"],
        cwd=tmp_path,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()

    manifest = ha._load_operator_manifest(tmp_path, trusted_head=head)

    assert manifest.commands[0].id == "pinned"


def test_preflight_reports_missing_manifest_in_history(
    tmp_path: pathlib.Path, monkeypatch
) -> None:
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha
    from runner.parser import parse

    _init_git_repo(tmp_path)
    subprocess.run(
        ["/usr/bin/git", "commit", "--allow-empty", "-qm", "empty"],
        cwd=tmp_path,
        check=True,
    )
    head = _repo_head(tmp_path)
    monkeypatch.setattr(ha, "_controller_trust_head", lambda workdir: head)
    graph = parse(ROOT / "pipelines" / "slim" / "two_node.dot")

    diagnostics = ha.validate_operator_trust_preflight(tmp_path, graph)

    assert diagnostics
    assert diagnostics[0]["code"] == "DF_OPERATOR_MANIFEST_MISSING_IN_HISTORY"
    assert ".github/dark-factory-operator.yaml" in diagnostics[0]["message"]


def test_preflight_passes_when_canonical_manifest_committed(
    tmp_path: pathlib.Path, monkeypatch
) -> None:
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha
    from runner.parser import parse

    _init_git_repo(tmp_path)
    _write_manifest(tmp_path / ".github" / "dark-factory-operator.yaml", _MANIFEST_BODY)
    subprocess.run(["/usr/bin/git", "add", "."], cwd=tmp_path, check=True)
    subprocess.run(
        ["/usr/bin/git", "commit", "-qm", "manifest"], cwd=tmp_path, check=True
    )
    head = _repo_head(tmp_path)
    monkeypatch.setattr(ha, "_controller_trust_head", lambda workdir: head)
    graph = parse(ROOT / "pipelines" / "slim" / "two_node.dot")

    diagnostics = ha.validate_operator_trust_preflight(tmp_path, graph)

    assert diagnostics == []


def test_cli_preflight_surfaces_operator_manifest_missing(
    tmp_path: pathlib.Path,
) -> None:
    _init_git_repo(tmp_path)
    subprocess.run(
        ["/usr/bin/git", "commit", "--allow-empty", "-qm", "empty"],
        cwd=tmp_path,
        check=True,
    )
    head = _repo_head(tmp_path)
    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner",
            "--pipeline",
            "two_node",
            "--workdir",
            str(tmp_path),
            "--preflight",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "DARK_FACTORY_HOME": str(ROOT),
            "DARK_FACTORY_OPERATOR_TRUST_HEAD": head,
        },
    )
    payload = json.loads(result.stdout)
    codes = [item["code"] for item in payload["diagnostics"]]

    assert result.returncode == 1
    assert "DF_OPERATOR_MANIFEST_MISSING_IN_HISTORY" in codes
