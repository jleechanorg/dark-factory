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


@pytest.mark.parametrize("backend", ["echo", "mock_llm"])
def test_operator_verify_synthesizes_cost_free_receipt_fixture(
    tmp_path: pathlib.Path, monkeypatch, backend: str
) -> None:
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha
    from runner.handler_core import Context
    from runner.parser import Node

    monkeypatch.setattr(
        ha,
        "run_bounded_process_bytes",
        lambda *args, **kwargs: (_ for _ in ()).throw(
            AssertionError("echo/mock must not launch project commands")
        ),
    )
    ctx = Context(goal="cost-free", workdir=tmp_path, backend=backend, run_id="echo")
    ctx.state["target_head_sha"] = "b" * 40

    result = ha._operator_verify(
        Node(name="operator_verify", attrs={"type": "operator_verify"}), ctx
    )

    assert result.outcome == "success"
    receipt = json.loads(
        (tmp_path / "evidence" / "operator-verification.json").read_text()
    )
    assert receipt["schema_version"] == 2
    assert receipt["target_head_sha"] == "b" * 40
    assert receipt["synthetic"] is True
    assert receipt["commands"] == []


def test_repository_operator_manifest_has_exact_sandbox_split() -> None:
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha

    manifest = ha._load_operator_manifest(ROOT)
    assert [command.id for command in manifest.commands] == [
        "worker-safe-targeted",
        "operator-unwrapped-six",
    ]
    worker, operator = manifest.commands
    assert worker.lane == "worker_safe"
    assert operator.lane == "operator_unwrapped"
    expected_nodes = [
        "tests/test_cli_fallbacks.py::test_controller_outer_sandbox_enforces_read_and_write_boundaries",
        "tests/test_cli_fallbacks.py::test_controller_outer_sandbox_avoids_nested_seatbelt",
        "tests/test_cli_fallbacks.py::test_controller_protects_linked_git_metadata_and_artifact_lane",
        "tests/test_ao_sandbox.py::test_fake_ao_shim_cannot_read_holdouts_under_sandbox",
        "tests/test_hardening.py::test_tool_handler_tolerates_bad_timeout",
        "tests/test_hardening.py::test_visible_all_nodes_benchmark_has_no_embedded_holdout_contract",
    ]
    assert [arg.removeprefix("--deselect=") for arg in worker.argv if arg.startswith("--deselect=")] == expected_nodes
    assert list(worker.argv[1:4]) == ["-q", "-o", "pythonpath=."]
    assert list(operator.argv[1:4]) == ["-q", "-o", "pythonpath=."]
    assert list(operator.argv[4:]) == expected_nodes
    assert [(item.id, item.classification) for item in manifest.exclusions] == [
        ("bounded-conformance", "excluded"),
        ("private-self-hosted-runner", "excluded"),
    ]


def test_receipt_validation_rejects_raw_logs_outside_private_root(
    tmp_path: pathlib.Path, monkeypatch
) -> None:
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha

    private_root = tmp_path / "approved"
    private_root.mkdir()
    outside = tmp_path / "outside.bin"
    outside.write_bytes(b"secret")
    digest = __import__("hashlib").sha256(b"secret").hexdigest()
    receipt = {
        "schema_version": 2,
        "target_head_sha": "c" * 40,
        "commands": [
            {
                "requested_argv": ["/usr/bin/git", "status"],
                "effective_argv": ["/usr/bin/git", "status"],
                "transform_chain": [],
                "stdout": {"path": str(outside), "sha256": digest, "size_bytes": 6},
                "stderr": {"path": str(outside), "sha256": digest, "size_bytes": 6},
            }
        ],
    }
    receipt_path = tmp_path / "receipt.json"
    receipt_path.write_text(json.dumps(receipt), encoding="utf-8")
    monkeypatch.setattr(ha, "_operator_log_root", lambda: private_root)

    with pytest.raises(ValueError, match="private root"):
        ha._validate_operator_receipt(receipt_path, "c" * 40)


def test_sensitive_binary_output_is_confined_to_private_raw_log(
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
    - id: secret-probe
      argv: [tools/fake-pytest]
      lane: operator_unwrapped
      timeout_seconds: 30
      classification: required
  exclusions: []
""",
    )

    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha
    from runner.handler_core import Context
    from runner.parser import Node
    from runner.subprocess_control import BoundedProcessBytesResult

    head = b"d" * 40 + b"\n"
    secret = b"SENTINEL-RAW-SECRET\x00\xff\n"
    outputs = iter(
        [head, b"changed.py\n", b"", b"", secret, head]
    )

    def fake_bounded(args, **kwargs):
        return BoundedProcessBytesResult(tuple(args), 0, next(outputs), b"", False)

    raw_root = tmp_path / "private"
    monkeypatch.setattr(ha, "run_bounded_process_bytes", fake_bounded)
    monkeypatch.setattr(ha, "_operator_log_root", lambda: raw_root)
    monkeypatch.setattr(ha._handlers_shim, "_sanitized_env", lambda: {})
    ctx = Context(goal="redact", workdir=tmp_path, backend="codex", run_id="secret")
    result = ha._operator_verify(
        Node(name="operator_verify", attrs={"type": "operator_verify"}), ctx
    )

    assert result.outcome == "success"
    receipt_path = tmp_path / "evidence" / "operator-verification.json"
    receipt_bytes = receipt_path.read_bytes()
    public_projection = (
        receipt_bytes
        + result.output.encode()
        + json.dumps(result.metadata).encode()
        + json.dumps(result.context_updates).encode()
        + json.dumps(ctx.state).encode()
    )
    assert b"SENTINEL-RAW-SECRET" not in public_projection
    command = json.loads(receipt_bytes)["commands"][4]
    raw_path = pathlib.Path(command["stdout"]["path"])
    assert raw_path.read_bytes() == secret
    assert command["stdout"]["size_bytes"] == len(secret)
    assert command["stdout"]["sha256"] == __import__("hashlib").sha256(secret).hexdigest()
    assert raw_path.stat().st_mode & 0o777 == 0o600


def test_receipt_validation_rejects_structural_tampering(
    tmp_path: pathlib.Path, monkeypatch
) -> None:
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha

    raw_root = tmp_path / "private"
    raw_root.mkdir()
    raw = raw_root / "stream.bin"
    raw.write_bytes(b"")
    empty_digest = __import__("hashlib").sha256(b"").hexdigest()
    expected = {
        "schema_version": 2,
        "target_head_sha": "e" * 40,
        "commands": [
            {
                "id": "git-status",
                "requested_argv": ["/usr/bin/git", "status", "--porcelain=v1"],
                "effective_argv": ["/usr/bin/git", "status", "--porcelain=v1"],
                "transform_chain": [],
                "stdout": {"path": str(raw), "sha256": empty_digest, "size_bytes": 0},
                "stderr": {"path": str(raw), "sha256": empty_digest, "size_bytes": 0},
            }
        ],
    }
    tampered = json.loads(json.dumps(expected))
    tampered["commands"] = []
    receipt_path = tmp_path / "receipt.json"
    receipt_path.write_text(json.dumps(tampered), encoding="utf-8")
    monkeypatch.setattr(ha, "_operator_log_root", lambda: raw_root)

    with pytest.raises(ValueError, match="tampered"):
        ha._validate_operator_receipt(
            receipt_path, "e" * 40, expected_receipt=expected
        )


def _run_fake_operator(
    tmp_path: pathlib.Path,
    monkeypatch,
    results,
    *,
    after_call=None,
    patch_raw_write=None,
):
    tool = tmp_path / "tools" / "fake-pytest"
    tool.parent.mkdir(parents=True)
    tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    tool.chmod(0o700)
    manifest = _write_operator_manifest(
        tmp_path,
        """\
operator_verification:
  schema_version: 1
  commands:
    - id: probe
      argv: [tools/fake-pytest]
      lane: operator_unwrapped
      timeout_seconds: 30
      classification: required
  exclusions: []
""",
    )
    import runner.handlers  # noqa: F401
    import runner.handler_audit as ha
    from runner.handler_core import Context
    from runner.parser import Node

    sequence = iter(results)
    calls = 0

    def fake_bounded(args, **kwargs):
        nonlocal calls
        calls += 1
        result = next(sequence)
        if after_call is not None:
            after_call(calls, manifest)
        return result

    monkeypatch.setattr(ha, "run_bounded_process_bytes", fake_bounded)
    monkeypatch.setattr(ha, "_operator_log_root", lambda: tmp_path / "private")
    monkeypatch.setattr(ha._handlers_shim, "_sanitized_env", lambda: {})
    if patch_raw_write is not None:
        patch_raw_write(ha, monkeypatch)
    ctx = Context(goal="fail closed", workdir=tmp_path, backend="codex", run_id="fail")
    return ha._operator_verify(
        Node(name="operator_verify", attrs={"type": "operator_verify"}), ctx
    )


def test_operator_verify_fails_closed_on_source_and_manifest_drift(
    tmp_path: pathlib.Path, monkeypatch
) -> None:
    from runner.subprocess_control import BoundedProcessBytesResult

    head = b"1" * 40 + b"\n"
    changed = b"2" * 40 + b"\n"
    ok = lambda output=b"": BoundedProcessBytesResult(("fake",), 0, output, b"", False)
    source_result = _run_fake_operator(
        tmp_path / "source",
        monkeypatch,
        [ok(head), ok(), ok(), ok(), ok(), ok(changed)],
    )
    assert source_result.outcome == "error"
    assert source_result.metadata["error_type"] == "source_head_drift"

    def drift_after_head(call, manifest):
        if call == 1:
            manifest.write_text(manifest.read_text() + "\n# drift\n", encoding="utf-8")

    manifest_result = _run_fake_operator(
        tmp_path / "manifest",
        monkeypatch,
        [ok(head)],
        after_call=drift_after_head,
    )
    assert manifest_result.outcome == "error"
    assert manifest_result.metadata["error_type"] == "ValueError"


def test_operator_verify_fails_closed_on_timeout_oversize_and_raw_write_failure(
    tmp_path: pathlib.Path, monkeypatch
) -> None:
    import runner.handler_audit as ha
    from runner.subprocess_control import BoundedProcessBytesResult

    head = b"3" * 40 + b"\n"
    ok = lambda output=b"": BoundedProcessBytesResult(("fake",), 0, output, b"", False)
    timeout = BoundedProcessBytesResult(("fake",), -15, b"partial", b"", True)
    timeout_result = _run_fake_operator(
        tmp_path / "timeout",
        monkeypatch,
        [ok(head), ok(), ok(), ok(), timeout],
    )
    assert timeout_result.outcome == "error"
    assert timeout_result.metadata["error_type"] == "timeout"

    nonzero = BoundedProcessBytesResult(("fake",), 2, b"", b"failed", False)
    nonzero_result = _run_fake_operator(
        tmp_path / "nonzero",
        monkeypatch,
        [ok(head), ok(), ok(), ok(), nonzero],
    )
    assert nonzero_result.outcome == "failure"
    assert nonzero_result.metadata["error_type"] == "nonzero_exit"

    oversized = ok(b"x" * (ha._OPERATOR_RAW_STREAM_MAX_BYTES + 1))
    oversize_result = _run_fake_operator(
        tmp_path / "oversize",
        monkeypatch,
        [ok(head), ok(), ok(), ok(), oversized],
    )
    assert oversize_result.outcome == "error"

    def fail_raw_write(module, mp):
        real_write = module._write_private_bytes

        def guarded(path, data):
            if "05-probe.stdout" in path.name:
                raise OSError("simulated raw-log write failure")
            return real_write(path, data)

        mp.setattr(module, "_write_private_bytes", guarded)

    write_result = _run_fake_operator(
        tmp_path / "write",
        monkeypatch,
        [ok(head), ok(), ok(), ok(), ok()],
        patch_raw_write=fail_raw_write,
    )
    assert write_result.outcome == "error"


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
