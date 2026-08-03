"""Tests for the vendor-neutral evidence-filename probe logic in runner/handler_audit.py.

Lane G (jleechan-9gi, audit-2026-06-27): the default probe list is no longer
Gemini-shaped. ``llm_request_responses.jsonl`` is the canonical default; any
project-local vendor alias (e.g. ``openai_request_responses.jsonl``) is added
via ``<workdir>/.dark-factory/evidence.yaml``.

NOTE on import ordering: handler_audit.py does
``import runner.handlers as _handlers_shim`` at module top, and runner.handlers
imports back from handler_audit. Importing handlers first warms sys.modules so
the cycle resolves cleanly.
"""
from __future__ import annotations

import json
import os
import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


def _make_ctx(tmp_path: pathlib.Path):
    import runner.handlers  # noqa: F401
    from runner.handlers import Context
    return Context(goal="lane-G audit test", workdir=tmp_path, backend="echo")


def _make_node(name: str = "audit_node"):
    import runner.handlers  # noqa: F401
    from runner.handlers import Node
    return Node(name=name, attrs={"type": "gate_audit", "shape": "hexagon"})


def _write_operator_manifest(tmp_path: pathlib.Path, body: str) -> pathlib.Path:
    manifest_dir = tmp_path / ".dark-factory"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    manifest = manifest_dir / "evidence.yaml"
    manifest.write_text(body, encoding="utf-8")
    return manifest


def test_operator_manifest_loads_only_exact_argv_entries(tmp_path: pathlib.Path) -> None:
    tool = tmp_path / "tools" / "fake-pytest"
    tool.parent.mkdir()
    tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    tool.chmod(0o700)
    _write_operator_manifest(
        tmp_path,
        """\
operator_verification:
  schema_version: 1
  commands:
    - id: worker-targeted
      argv: [tools/fake-pytest, -q, tests/test_one.py]
      lane: worker_safe
      timeout_seconds: 60
      classification: required
  exclusions:
    - id: bounded-conformance
      classification: excluded
      reason: separately bounded acceptance lane
""",
    )

    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha

    manifest = ha._load_operator_manifest(tmp_path)

    assert manifest.schema_version == 1
    assert manifest.commands[0].argv == (
        str(tool.resolve()),
        "-q",
        "tests/test_one.py",
    )
    assert manifest.commands[0].lane == "worker_safe"
    assert manifest.exclusions[0].classification == "excluded"
    assert os.access(manifest.commands[0].argv[0], os.X_OK)


@pytest.mark.parametrize(
    "mutated",
    [
        "shell_string",
        "unknown_command_key",
        "unknown_lane",
        "duplicate_id",
        "path_escape",
        "unsafe_absolute_executable",
        "invalid_timeout",
        "symlink_executable",
        "malformed_yaml",
    ],
)
def test_operator_manifest_rejects_injection_and_ambiguity(
    tmp_path: pathlib.Path, mutated: str
) -> None:
    tool = tmp_path / "tools" / "fake-pytest"
    tool.parent.mkdir()
    tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    tool.chmod(0o700)
    command = """\
    - id: worker-targeted
      argv: [tools/fake-pytest, -q]
      lane: worker_safe
      timeout_seconds: 60
      classification: required
"""
    if mutated == "shell_string":
        command = command.replace("[tools/fake-pytest, -q]", "tools/fake-pytest -q")
    elif mutated == "unknown_command_key":
        command += "      shell: true\n"
    elif mutated == "unknown_lane":
        command = command.replace("worker_safe", "ambient_path")
    elif mutated == "duplicate_id":
        command += command
    elif mutated == "path_escape":
        command = command.replace("tools/fake-pytest", "../fake-pytest")
    elif mutated == "unsafe_absolute_executable":
        command = command.replace("tools/fake-pytest", "/bin/sh")
    elif mutated == "invalid_timeout":
        command = command.replace("timeout_seconds: 60", "timeout_seconds: 0")
    elif mutated == "symlink_executable":
        link = tmp_path / "tools" / "linked-pytest"
        link.symlink_to(tool)
        command = command.replace("tools/fake-pytest", "tools/linked-pytest")

    body = f"""\
operator_verification:
  schema_version: 1
  commands:
{command}  exclusions: []
"""
    if mutated == "malformed_yaml":
        body = "operator_verification: [unterminated"
    _write_operator_manifest(tmp_path, body)

    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha

    with pytest.raises(ValueError):
        ha._load_operator_manifest(tmp_path)


def test_operator_verify_runs_exact_unwrapped_argv_and_writes_receipt_v2(
    tmp_path: pathlib.Path, monkeypatch
) -> None:
    tool = tmp_path / "tools" / "fake-pytest"
    tool.parent.mkdir()
    tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    tool.chmod(0o700)
    _write_operator_manifest(
        tmp_path,
        """\
operator_verification:
  schema_version: 1
  commands:
    - id: worker-targeted
      argv: [tools/fake-pytest, -q, tests/test_one.py]
      lane: worker_safe
      timeout_seconds: 60
      classification: required
  exclusions: []
""",
    )

    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha
    from runner.handler_core import Context
    from runner.parser import Node
    from runner.subprocess_control import BoundedProcessBytesResult

    head = "a" * 40
    calls: list[tuple[tuple[str, ...], dict]] = []
    outputs = iter(
        [
            (head.encode() + b"\n", b""),
            (b"runner/handler_audit.py\n", b""),
            (b"", b""),
            (b"", b""),
            (b"1 passed\n", b""),
            (head.encode() + b"\n", b""),
        ]
    )

    def fake_bounded(args, **kwargs):
        calls.append((tuple(args), kwargs))
        stdout, stderr = next(outputs)
        return BoundedProcessBytesResult(tuple(args), 0, stdout, stderr, False)

    raw_root = tmp_path / "private-operator-logs"
    monkeypatch.setattr(ha, "run_bounded_process_bytes", fake_bounded, raising=False)
    monkeypatch.setattr(ha, "_operator_log_root", lambda: raw_root, raising=False)
    monkeypatch.setattr(ha._handlers_shim, "_sanitized_env", lambda: {"SAFE": "1"})
    ctx = Context(
        goal="verify exact commands",
        workdir=tmp_path,
        backend="codex",
        run_id="run-1",
    )
    ctx._df_current_attempt = 1

    result = ha._operator_verify(
        Node(name="operator_verify", attrs={"type": "operator_verify"}), ctx
    )

    assert result.outcome == "success", result.output
    assert [call[0] for call in calls[:4]] == [
        ("/usr/bin/git", "rev-parse", "HEAD"),
        ("/usr/bin/git", "diff", "--name-only", "origin/main..HEAD"),
        ("/usr/bin/git", "diff", "--check", "origin/main..HEAD"),
        ("/usr/bin/git", "status", "--porcelain=v1"),
    ]
    assert calls[4][0] == (str(tool.resolve()), "-q", "tests/test_one.py")
    assert calls[-1][0] == ("/usr/bin/git", "rev-parse", "HEAD")
    assert all(call[1]["cwd"] == tmp_path.resolve() for call in calls)
    assert all(call[1]["env"] == {"SAFE": "1"} for call in calls)

    receipt_path = tmp_path / "evidence" / "operator-verification.json"
    receipt = json.loads(receipt_path.read_text())
    assert receipt["schema_version"] == 2
    assert receipt["target_head_sha"] == head
    assert [command["id"] for command in receipt["commands"]] == [
        "git-head",
        "git-diff-names",
        "git-diff-check",
        "git-status",
        "worker-targeted",
        "git-head-final",
    ]
    status = receipt["commands"][3]
    assert status["requested_argv"] == status["effective_argv"]
    assert status["transform_chain"] == []
    assert status["stdout"]["size_bytes"] == 0


def test_default_probe_list_is_vendor_neutral():
    import runner.handlers  # noqa: F401
    from runner.handler_audit import DEFAULT_EVIDENCE_FILENAMES
    joined = "\n".join(DEFAULT_EVIDENCE_FILENAMES).lower()
    assert "gemini" not in joined, (
        f"vendor-shaped default leaked into DEFAULT_EVIDENCE_FILENAMES: {DEFAULT_EVIDENCE_FILENAMES}"
    )
    assert "llm_request_responses.jsonl" in DEFAULT_EVIDENCE_FILENAMES
    assert "evidence.jsonl" in DEFAULT_EVIDENCE_FILENAMES


def test_alias_yml_adds_vendor_specific_filename(tmp_path: pathlib.Path, monkeypatch):
    """Worktree with both ``openai_request_responses.jsonl`` + ``llm_request_responses.jsonl``
    plus an alias YAML pointing at the openai file must have BOTH probed."""
    (tmp_path / "openai_request_responses.jsonl").write_text("payload\n")
    (tmp_path / "llm_request_responses.jsonl").write_text("payload\n")

    manifest_dir = tmp_path / ".dark-factory"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    (manifest_dir / "evidence.yaml").write_text(
        "aliases:\n  - openai_request_responses.jsonl\n",
        encoding="utf-8",
    )

    import runner.handler_audit as ha
    monkeypatch.setattr(ha, "_git_config_origin_url", lambda *a, **k: "N/A")
    monkeypatch.setattr(ha, "_git_merge_base", lambda *a, **k: "")
    monkeypatch.setattr(ha, "_check_unresolved_review_state", lambda *a, **k: True)

    from runner.handlers import _gate_audit
    node = _make_node()
    ctx = _make_ctx(tmp_path)

    res = _gate_audit(node, ctx)
    assert res.outcome == "failure", res.output
    assert "stale evidence" in res.output, res.output
    assert "missing evidence artifacts" not in res.output, (
        f"openai_request_responses.jsonl was not probed despite evidence.yaml alias:\n{res.output}"
    )

    verdict = json.loads((tmp_path / "gate_audit_verdict.json").read_text())
    assert "openai_request_responses.jsonl" in verdict["evidence_paths"], verdict
    assert "llm_request_responses.jsonl" in verdict["evidence_paths"], verdict


def test_openai_file_alone_is_probed_via_yaml(tmp_path: pathlib.Path, monkeypatch):
    """Worktree has ONLY the vendor-shaped file. The alias YAML must promote it
    onto the probe list, otherwise the audit fails with 'missing evidence artifacts'."""
    (tmp_path / "openai_request_responses.jsonl").write_text("payload\n")

    manifest_dir = tmp_path / ".dark-factory"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    (manifest_dir / "evidence.yaml").write_text(
        "aliases:\n  - openai_request_responses.jsonl\n",
        encoding="utf-8",
    )

    import runner.handler_audit as ha
    monkeypatch.setattr(ha, "_git_config_origin_url", lambda *a, **k: "N/A")
    monkeypatch.setattr(ha, "_git_merge_base", lambda *a, **k: "")
    monkeypatch.setattr(ha, "_check_unresolved_review_state", lambda *a, **k: True)

    from runner.handlers import _gate_audit
    node = _make_node()
    ctx = _make_ctx(tmp_path)

    res = _gate_audit(node, ctx)
    assert "missing evidence artifacts" not in res.output, (
        f"openai_request_responses.jsonl alone should be probed via evidence.yaml alias:\n{res.output}"
    )
    assert "stale evidence" in res.output, res.output


def test_gemini_alias_still_supported_via_yaml(tmp_path: pathlib.Path, monkeypatch):
    """Backwards compat: ``gemini_http_request_responses.jsonl`` still works when
    added to ``.dark-factory/evidence.yaml``."""
    (tmp_path / "gemini_http_request_responses.jsonl").write_text("payload\n")

    manifest_dir = tmp_path / ".dark-factory"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    (manifest_dir / "evidence.yaml").write_text(
        "aliases:\n  - gemini_http_request_responses.jsonl\n",
        encoding="utf-8",
    )

    import runner.handler_audit as ha
    monkeypatch.setattr(ha, "_git_config_origin_url", lambda *a, **k: "N/A")
    monkeypatch.setattr(ha, "_git_merge_base", lambda *a, **k: "")
    monkeypatch.setattr(ha, "_check_unresolved_review_state", lambda *a, **k: True)

    from runner.handlers import _gate_audit
    node = _make_node()
    ctx = _make_ctx(tmp_path)

    res = _gate_audit(node, ctx)
    assert "missing evidence artifacts" not in res.output, res.output
    assert "stale evidence" in res.output, res.output


def test_no_yaml_no_legacy_gemini_probe(tmp_path: pathlib.Path):
    """Without an evidence.yaml manifest, the runner must NOT probe
    ``gemini_http_request_responses.jsonl`` even if such a file exists."""
    import runner.handlers  # noqa: F401
    from runner.handler_audit import _load_evidence_aliases
    (tmp_path / "gemini_http_request_responses.jsonl").write_text("payload\n")
    (tmp_path / "llm_request_responses.jsonl").write_text("payload\n")

    assert _load_evidence_aliases(tmp_path) == [], "aliases should be empty without evidence.yaml"
