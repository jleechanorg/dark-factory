"""Parity: Rust daemon, Python gate, and JSON config share reviewer priority.

`skeptic_reviewer_priority()` resolves to one of two lists depending on
`DARK_FACTORY_VIA_AF`: the daemon sets this on every AO worker it spawns for
automated /af bead dispatch (see `ao_spawn_command_with_mode` in
`daemon/src/adapters.rs`), so its presence means "this process tree was
launched by the /af daemon." Absence (a human running `dark-factory`/
`/factory` directly, or any other caller) resolves to the manual list.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from runner.reviewer_priority import (
    coder_fallback_chain,
    default_coder,
    default_reviewers_json,
    mandatory_reviewers,
    skeptic_reviewer_priority,
)

_CONFIG = (
    Path(__file__).resolve().parent.parent / "config" / "skeptic_reviewer_priority.json"
)
_RAW = json.loads(_CONFIG.read_text(encoding="utf-8"))


def test_json_file_matches_python_loader_for_af_context(monkeypatch):
    monkeypatch.setenv("DARK_FACTORY_VIA_AF", "1")
    assert skeptic_reviewer_priority() == _RAW["reviewer_priority"]
    assert default_coder() == _RAW.get("default_coder", "agy")
    assert coder_fallback_chain() == _RAW.get("coder_fallback_chain", ["claudem"])


def test_json_file_matches_python_loader_for_manual_context(monkeypatch):
    monkeypatch.delenv("DARK_FACTORY_VIA_AF", raising=False)
    assert skeptic_reviewer_priority() == _RAW["reviewer_priority_manual"]


def test_mandatory_reviewers_equals_priority_list(monkeypatch):
    monkeypatch.setenv("DARK_FACTORY_VIA_AF", "1")
    assert mandatory_reviewers() == tuple(skeptic_reviewer_priority())


def test_default_reviewers_json_covers_all_vendors(monkeypatch):
    monkeypatch.setenv("DARK_FACTORY_VIA_AF", "1")
    parsed = json.loads(default_reviewers_json())
    ids = [pair[0] for pair in parsed]
    assert ids == skeptic_reviewer_priority()


def test_af_priority_excludes_codex(monkeypatch):
    """The /af daemon path (DARK_FACTORY_VIA_AF set) stays claudem-first —
    codex is deliberately excluded there to conserve codex quota for
    interactive/manual use."""
    monkeypatch.setenv("DARK_FACTORY_VIA_AF", "1")
    priority = skeptic_reviewer_priority()
    assert priority == ["claudem", "agy", "cursor-agent"]
    assert "codex" not in priority
    assert "gemini" not in priority


def test_manual_priority_is_codex_first(monkeypatch):
    """A manual `/factory`/`dark-factory` invocation (no DARK_FACTORY_VIA_AF)
    puts codex first, per operator intent."""
    monkeypatch.delenv("DARK_FACTORY_VIA_AF", raising=False)
    priority = skeptic_reviewer_priority()
    assert priority == ["codex", "claudem", "agy", "claude"]
    assert priority[0] == "codex"


@pytest.mark.parametrize("via_af_value", ["", "0", "false"])
def test_falsy_via_af_values_resolve_to_manual(monkeypatch, via_af_value):
    """Only a truthy DARK_FACTORY_VIA_AF selects the /af list; an empty or
    explicitly-falsy value must not accidentally select it."""
    monkeypatch.setenv("DARK_FACTORY_VIA_AF", via_af_value)
    assert skeptic_reviewer_priority() == _RAW["reviewer_priority_manual"]


def test_missing_reviewer_priority_manual_key_fails_closed(monkeypatch):
    import runner.reviewer_priority as rp

    monkeypatch.delenv("DARK_FACTORY_VIA_AF", raising=False)
    monkeypatch.setattr(rp, "_load", lambda: {"reviewer_priority": ["claudem"]})
    with pytest.raises(ValueError, match="reviewer_priority_manual"):
        rp.skeptic_reviewer_priority()


def test_malformed_reviewer_priority_manual_fails_closed(monkeypatch):
    import runner.reviewer_priority as rp

    monkeypatch.delenv("DARK_FACTORY_VIA_AF", raising=False)
    monkeypatch.setattr(
        rp, "_load", lambda: {"reviewer_priority": ["claudem"], "reviewer_priority_manual": []}
    )
    with pytest.raises(ValueError, match="reviewer_priority_manual"):
        rp.skeptic_reviewer_priority()


def test_skeptic_gate_workflow_sets_via_af_for_ci_gate():
    """The GHA skeptic gate is an unattended CI job, not a manual/interactive
    `/factory` invocation -- without DARK_FACTORY_VIA_AF, skeptic_gate_cli.py's
    chain-walk premium reviewer would silently resolve to codex (the manual
    list's head), burning codex quota on every gated PR post-merge, which is
    exactly what the /af-vs-manual split exists to prevent."""
    import yaml

    workflow_path = (
        Path(__file__).resolve().parent.parent / ".github" / "workflows" / "skeptic-gate.yml"
    )
    workflow = yaml.safe_load(workflow_path.read_text(encoding="utf-8"))
    jobs = workflow.get("jobs", {})
    assert "skeptic" in jobs, "expected a 'skeptic' job in skeptic-gate.yml"
    job_env = jobs["skeptic"].get("env", {})
    assert str(job_env.get("DARK_FACTORY_VIA_AF", "")).strip().lower() not in (
        "",
        "0",
        "false",
        "no",
        "off",
    ), "skeptic job must set a truthy DARK_FACTORY_VIA_AF so the CI gate stays claudem-first"
