"""PR #819 round-2 adversarial findings: the chain-walk priority queue in
``runner/dispatcher.py:VerifierDispatcher._chain_walk_reviewer`` reads
``skeptic_reviewer_priority()`` (``config/skeptic_reviewer_priority.json`` =
``["claudem", "agy", "cursor-agent"]``), but two pieces of the gate never
learned about those vendor names:

1. ``runner/skeptic_gate_cli._build_reviewer_cmd`` only builds argv for
   ``codex``/``gemini``. When the chain-walk advances to ``claudem`` (e.g.
   after a codex rate-limit bust), ``invoke_reviewer`` raises
   ``RuntimeError("unknown reviewer 'claudem'; expected 'codex' or
   'gemini'")``. That message does not match ``_detect_rate_limit``'s
   patterns, so the chain-walk's ``continue`` branch is never taken — the
   walk stops dead at the first configured fallback vendor instead of
   advancing past it, defeating the whole point of the priority queue.

2. ``runner.skeptic_gate.parse_verdict``'s ``_IDENTITY_RE`` and the
   ``identity_token not in (...)`` check hardcode
   ``claude|codex|gemini|human|unknown`` — so even a reviewer that
   correctly declares its own vendor name (``IDENTITY: claudem``, as
   ``tests/test_dispatcher_reviewer_chain_walk.py``'s own ``VALID_OUTPUT``
   fixture does) is rejected by the deterministic parser BEFORE
   ``bind_reviewer_identity`` (which already supports claudem/agy/
   cursor-agent via ``_extend_bind_table_from_priority()``) ever runs.

Both defects mean the chain-walk fallback added in PR #654 has never
actually worked end-to-end for the real configured vendors.
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner.reviewer_priority import skeptic_reviewer_priority  # noqa: E402
from runner.skeptic_gate import (  # noqa: E402
    REVIEWER_CLI_TO_IDENTITY,
    bind_reviewer_identity,
    parse_verdict,
)
from runner.skeptic_gate_cli import _build_reviewer_cmd  # noqa: E402


def _verdict_text(identity: str) -> str:
    return (
        "VERDICT: PASS\n"
        "HEAD_SHA: 0123456789abcdef0123456789abcdef01234567\n"
        "REPO: jleechanorg/dark-factory\n"
        "PR_NUMBER: 819\n"
        "REASON: lgtm\n"
        f"IDENTITY: {identity}\n"
        "TEST_RUN_EVIDENCE: passed=10 failed=0 skipped=0 exit=0\n"
        "LINT_RUN_EVIDENCE: tool=ruff errors=0 warnings=0\n"
        "GREP_CITES: runner/foo.py:42\n"
        "HEAD_COMMIT_VERIFIED: 0123456789abcdef0123456789abcdef01234567\n"
    )


@pytest.mark.parametrize("vendor", list(skeptic_reviewer_priority()))
def test_build_reviewer_cmd_supports_every_configured_priority_vendor(vendor):
    """Defect 1: every vendor in the live priority config must build a
    real argv, not raise ``RuntimeError("unknown reviewer ...")``.

    This is the exact failure that stops the chain-walk cold: the
    RuntimeError text does not match ``_detect_rate_limit``'s patterns,
    so ``_chain_walk_reviewer`` treats it as a terminal (non-rate-limit)
    outcome instead of advancing to the next vendor in the queue.
    """
    cmd = _build_reviewer_cmd(vendor, "")
    assert isinstance(cmd, list) and cmd, (
        f"_build_reviewer_cmd({vendor!r}) must return a non-empty argv"
    )


def test_build_reviewer_cmd_still_rejects_a_truly_unknown_vendor():
    """A vendor NOT in the priority config (and not codex/gemini) should
    still fail loudly — this is not a request to accept arbitrary strings."""
    with pytest.raises(RuntimeError):
        _build_reviewer_cmd("totally-not-a-reviewer", "")


@pytest.mark.parametrize("vendor", list(skeptic_reviewer_priority()))
def test_parse_verdict_accepts_declared_identity_for_every_priority_vendor(vendor):
    """Defect 2: parse_verdict must not reject a verdict whose IDENTITY
    line is the reviewer's own configured vendor name.

    ``tests/test_dispatcher_reviewer_chain_walk.py``'s ``VALID_OUTPUT``
    fixture already declares ``IDENTITY: claudem`` — that fixture is only
    ever fed through a *mocked* ``evaluate()``, so this was never
    exercised against the real deterministic parser until now.
    """
    parsed = parse_verdict(_verdict_text(vendor))
    assert parsed is not None, (
        f"parse_verdict rejected a verdict declaring IDENTITY: {vendor} "
        "even though that is a live entry in skeptic_reviewer_priority()"
    )
    assert parsed.reviewer_identity == vendor.lower()


@pytest.mark.parametrize("vendor", list(skeptic_reviewer_priority()))
def test_bind_reviewer_identity_end_to_end_for_every_priority_vendor(vendor):
    """Full loop: parse_verdict's accepted identity must also satisfy
    bind_reviewer_identity (already extended via
    _extend_bind_table_from_priority — this asserts the two layers agree)."""
    parsed = parse_verdict(_verdict_text(vendor))
    assert parsed is not None
    ok, why = bind_reviewer_identity(vendor, parsed.reviewer_identity)
    assert ok, f"bind_reviewer_identity({vendor!r}, {parsed.reviewer_identity!r}) failed: {why}"
    assert REVIEWER_CLI_TO_IDENTITY.get(vendor) == vendor
