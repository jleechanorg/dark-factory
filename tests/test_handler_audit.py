"""Tests for the vendor-neutral evidence-filename probe logic in runner/handler_audit.py.

Lane G (jleechan-9gi, audit-2026-06-27): the default probe list is no longer
Gemini-shaped. ``llm_request_responses.jsonl`` is the canonical default; any
project-local vendor alias (e.g. ``openai_request_responses.jsonl``) is added
via ``<workdir>/.dark-factory/evidence.yaml``.

NOTE on import ordering: handler_audit.py does
``import runner.handlers as _handlers_shim`` at module top, and runner.handlers
imports back from handler_audit. Importing handlers first warms sys.modules so
the cycle resolves cleanly — see the imports below.
"""
from __future__ import annotations

import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


def _make_ctx(tmp_path: pathlib.Path):
    import runner.handlers  # noqa: F401  -- break circular import
    from runner.handlers import Context
    return Context(goal="lane-G audit test", workdir=tmp_path, backend="echo")


def _make_node(name: str = "audit_node"):
    import runner.handlers  # noqa: F401  -- break circular import
    from runner.handlers import Node
    # No evidence_paths attr -> exercises the default probe list path.
    return Node(name=name, attrs={"type": "gate_audit", "shape": "hexagon"})


def test_default_probe_list_is_vendor_neutral():
    """The hard-coded default list must not contain any Gemini-shaped names."""
    # Import handler_audit AFTER runner.handlers to break the circular-init ordering.
    import runner.handlers  # noqa: F401
    from runner.handler_audit import DEFAULT_EVIDENCE_FILENAMES
    joined = "\n".join(DEFAULT_EVIDENCE_FILENAMES).lower()
    assert "gemini" not in joined, (
        f"vendor-shaped default leaked into DEFAULT_EVIDENCE_FILENAMES: {DEFAULT_EVIDENCE_FILENAMES}"
    )
    # Spot-check the canonical defaults the audit agreed on.
    assert "llm_request_responses.jsonl" in DEFAULT_EVIDENCE_FILENAMES
    assert "evidence.jsonl" in DEFAULT_EVIDENCE_FILENAMES


def test_alias_yml_adds_vendor_specific_filename(tmp_path: pathlib.Path, monkeypatch):
    """A worktree with ``openai_request_responses.jsonl`` + ``llm_request_responses.jsonl``
    plus an alias YAML pointing at the openai file must have BOTH probed."""
    # Drop a vendor-named evidence file the project considers canonical.
    (tmp_path / "openai_request_responses.jsonl").write_text("payload\n")
    # And drop the new vendor-neutral default file alongside it.
    (tmp_path / "llm_request_responses.jsonl").write_text("payload\n")

    # Tell the runner about the project's vendor alias.
    manifest_dir = tmp_path / ".dark-factory"
    manifest_dir.mkdir(parents=True, exist_ok=True)
    (manifest_dir / "evidence.yaml").write_text(
        "aliases:\n  - openai_request_responses.jsonl\n",
        encoding="utf-8",
    )

    # Avoid hitting gh / git during this test.
    import runner.handler_audit as ha
    monkeypatch.setattr(ha, "_git_config_origin_url", lambda *a, **k: "N/A")
    monkeypatch.setattr(ha, "_git_merge_base", lambda *a, **k: "")
    monkeypatch.setattr(ha, "_check_unresolved_review_state", lambda *a, **k: True)

    from runner.handlers import _gate_audit
    node = _make_node()
    ctx = _make_ctx(tmp_path)

    res = _gate_audit(node, ctx)
    # The run fails on stale evidence (no head_sha in the file), not on missing
    # artifacts — both files must have been probed.
    assert res.outcome == "failure", res.output
    assert "stale evidence" in res.output, res.output
    assert "missing evidence artifacts" not in res.output, (
        f"openai_request_responses.jsonl was not probed despite evidence.yaml alias:\n{res.output}"
    )

    verdict = json.loads((tmp_path / "gate_audit_verdict.json").read_text())
    assert "openai_request_responses.jsonl" in verdict["evidence_paths"], verdict
    assert "llm_request_responses.jsonl" in verdict["evidence_paths"], verdict


def test_openai_file_alone_is_probed_via_yaml(tmp_path: pathlib.Path, monkeypatch):
    """Worktree has ONLY the vendor-shaped file (no default-named file). The alias YAML
    must promote it onto the probe list, otherwise the audit fails with
    'missing evidence artifacts' rather than 'stale evidence'."""
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
    """Backwards compatibility: a project that already uses ``gemini_http_request_responses.jsonl``
    can keep working by adding it to ``.dark-factory/evidence.yaml``."""
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
    ``gemini_http_request_responses.jsonl`` even if such a file exists —
    that was the original bias."""
    import runner.handlers  # noqa: F401  -- break circular import
    from runner.handler_audit import _load_evidence_aliases
    # Legacy Gemini file present but NO yaml manifest.
    (tmp_path / "gemini_http_request_responses.jsonl").write_text("payload\n")
    # New vendor-neutral file also present.
    (tmp_path / "llm_request_responses.jsonl").write_text("payload\n")

    assert _load_evidence_aliases(tmp_path) == [], "aliases should be empty without evidence.yaml"