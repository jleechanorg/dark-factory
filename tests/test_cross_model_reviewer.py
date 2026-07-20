"""Cross-model reviewer vendor-fallback chain (issue #385 / bead jleechan-984e).

Adds:
  * `runner.cross_model_reviewer` — pure-logic module owning the
    cursor-agent → codex → agy vendor chain, the differ-from-coder-family
    rule, and the `review_degraded` flag.
  * `gate_cross_model` node type registered in `handlers.TYPE_REGISTRY`
    that wires the chain into the runner + emits a verdict into the
    CXDB/perf_log envelope via `Result.metadata`.

Existing modules reused (no duplication):
  * `_probe_backend_installed` from `runner.handler_dispatch` for vendor
    probe semantics (PATH-based `which` + cheap `--version`).
  * `_parse_verdict` from `runner.handler_verdict` for verdict-token
    normalization. The cross-model reviewer is the SAME kind of gate
    output as `gate_es` / `gate_skeptic`; verdict semantics must not
    drift.
  * `invoke_reviewer` from `runner.skeptic_gate_cli` for the sandboxed
    CLI subprocess envelope. Cross-model review uses sanitized env +
    read-only sandbox identical to the skeptic gate.

These tests are the ironclad exit criteria — passing all of them is a
hard precondition for claiming the cross-model reviewer is shipped.

Design contract (canonical reference, do NOT inline these rules in
consumers):
  * Chain order: ``cursor-agent`` → ``codex`` → ``agy``.
  * ``review_degraded`` is ``True`` iff only one model family ran.
  * Differ-from-coder rule: when the coder's family has a sibling in
    the chain, prefer that sibling over same-family entries.
  * Empty / unknown vendor chain entries → fail closed (return
    ``None``, do NOT invent a backend).
"""

from __future__ import annotations

import importlib
import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))


def _load_cross_model_module():
    """Late-bound import so monkeypatches on dispatcher._probe_backend_installed
    take effect even when probe_fn is implicit."""
    return importlib.import_module("runner.cross_model_reviewer")


# ---------------------------------------------------------------------------
# Vendor fallback chain (cursor-agent → codex → agy)
# ---------------------------------------------------------------------------


def test_vendor_chain_resolves_first_installed_cursor_agent(monkeypatch):
    """cursor-agent installed → resolver picks it as the first entry."""
    cm = _load_cross_model_module()

    def fake_probe(name):
        return name == "cursor-agent"

    resolution = cm.resolve_cross_model_reviewer(
        coder_family="anthropic", probe_fn=fake_probe
    )
    assert resolution.vendor == "cursor-agent"
    assert resolution.family == "cursor"
    assert resolution.degraded is False, (
        "two-family default expectation must hold: cursor-agent family "
        "differs from coder anthropic family"
    )
    # Entries past the resolution point are NOT recorded as skipped
    # (they are not in the resolution set, only entries that were
    # attempted-but-failed are).
    assert resolution.skipped == ()


def test_vendor_chain_falls_through_to_codex_when_cursor_missing(monkeypatch):
    """cursor-agent missing → fall through to codex, mark cursor as skipped."""
    cm = _load_cross_model_module()

    def fake_probe(name):
        return name == "codex"

    resolution = cm.resolve_cross_model_reviewer(
        coder_family="anthropic", probe_fn=fake_probe
    )
    assert resolution.vendor == "codex"
    assert resolution.family == "openai"
    assert resolution.skipped == ("cursor-agent",)
    assert resolution.degraded is False, "codex (openai) differs from anthropic"


def test_vendor_chain_falls_through_to_agy_when_cursor_and_codex_missing(monkeypatch):
    """Full fall-through → cursor + codex skipped, agy wins."""
    cm = _load_cross_model_module()

    def fake_probe(name):
        return name == "agy"

    resolution = cm.resolve_cross_model_reviewer(
        coder_family="anthropic", probe_fn=fake_probe
    )
    assert resolution.vendor == "agy"
    assert resolution.family == "google"
    assert resolution.skipped == ("cursor-agent", "codex")
    assert resolution.degraded is False, "agy (google) differs from anthropic"


def test_vendor_chain_returns_none_when_nothing_installed(monkeypatch):
    """No vendor in the chain installed → fail closed (None), no invented backend."""
    cm = _load_cross_model_module()

    def fake_probe(_name):
        return False

    resolution = cm.resolve_cross_model_reviewer(
        coder_family="anthropic", probe_fn=fake_probe
    )
    assert resolution.vendor is None
    assert resolution.family is None
    assert resolution.skipped == cm.CROSS_MODEL_VENDOR_CHAIN
    assert resolution.degraded is True, (
        "no vendor ran → degraded must be true (strict merge policy #328 treats "
        "this as NOT strict-green)"
    )


# ---------------------------------------------------------------------------
# Differ-from-coder-family rule (the central invariant)
# ---------------------------------------------------------------------------


def test_vendor_chain_prefers_cross_family_when_coder_same_family(monkeypatch):
    """When the coder's family matches an entry, prefer a cross-family entry.

    Cursor's model is from a different family than Claude; when the coder is
    already cursor-family, the next non-cursor entry must win.
    """
    cm = _load_cross_model_module()

    def fake_probe(name):
        # cursor-agent installed + agy installed; chain order means cursor is
        # first but the coder is already cursor-family, so agy wins.
        return name in {"cursor-agent", "agy"}

    resolution = cm.resolve_cross_model_reviewer(
        coder_family="cursor", probe_fn=fake_probe
    )
    assert resolution.vendor == "agy", (
        "cross-family rule should escalate past cursor when coder is cursor-family"
    )
    assert resolution.family == "google"
    # ``skipped`` is the audit trail of every entry probed that did NOT
    # resolve: cursor-agent (rejected for cross-family escalation) AND
    # codex (uninstalled, bypassed). Resolution lands on agy.
    assert "cursor-agent" in resolution.skipped
    assert "codex" in resolution.skipped
    assert "agy" not in resolution.skipped, (
        "agy is the resolved vendor; it must NOT appear in skipped"
    )


def test_vendor_chain_marks_degraded_when_only_coder_family_available(monkeypatch):
    """Single-family chain → `degraded=True`, the gate still picks the best
    available entry but the flag travels with the result.

    Strict-merge policy #328 reads `degraded=True` and refuses strict-green.
    """
    cm = _load_cross_model_module()

    def fake_probe(name):
        return name == "codex"

    resolution = cm.resolve_cross_model_reviewer(
        coder_family="openai", probe_fn=fake_probe
    )
    assert resolution.vendor == "codex"
    assert resolution.family == "openai"
    assert resolution.degraded is True, (
        "codex/openai same-family as coder/openai → review is degraded (single "
        "family); strict-merge #328 must refuse strict-green on this run"
    )


# ---------------------------------------------------------------------------
# Verdict semantics — reuse `_parse_verdict`, do not duplicate token map
# ---------------------------------------------------------------------------


def test_aggregate_verdict_uses_existing_parse_verdict_semantics(monkeypatch):
    """`aggregate_cross_model_verdict` must route through `_parse_verdict` so
    verdict-token semantics stay in one module. A review that emits
    `verdict: pass` normalizes to ``success``; `verdict: fail` → ``failure``.
    """
    cm = _load_cross_model_module()

    out = cm.aggregate_cross_model_verdict(
        "verdict: pass\nhead_sha: " + "a" * 40,
        gate_strict=False,
    )
    assert out.raw_verdict == "pass"
    assert out.normalized_outcome == "success", (
        "verdict: pass MUST normalize to success via the canonical _parse_verdict"
    )

    out_fail = cm.aggregate_cross_model_verdict(
        "verdict: fail\nhead_sha: " + "b" * 40,
        gate_strict=False,
    )
    assert out_fail.normalized_outcome == "failure"


def test_aggregate_verdict_unknown_token_fails_closed(monkeypatch):
    """An ambiguous / unparseable review output MUST fail closed: outcome=
    ``failure`` even when rc=0. We do NOT default to success on missing
    verdict tokens — that is the misclassification the gate hardening
    tests forbid."""
    cm = _load_cross_model_module()

    out = cm.aggregate_cross_model_verdict(
        "reviewer emitted no verdict marker at all\njust prose here",
        gate_strict=False,
    )
    assert out.raw_verdict == "unknown"
    assert out.normalized_outcome == "failure"


# ---------------------------------------------------------------------------
# Result metadata telemetry contract (CXDB / perf_log keys)
# ---------------------------------------------------------------------------


def test_degraded_flag_propagates_into_result_metadata():
    """The runner emits `cross_model_degraded` into `Result.metadata` (which
    feeds CXDB + perf_log) so the strict-merge policy can read it without
    parsing free-form output."""
    cm = _load_cross_model_module()

    fake_resolution = cm.CrossModelResolution(
        vendor="codex",
        family="openai",
        skipped=("cursor-agent",),
        degraded=True,
    )

    fake_verdict = cm.CrossModelVerdict(
        raw_verdict="pass",
        normalized_outcome="success",
        parsed_text="verdict: pass",
    )

    meta = cm.build_metadata(resolution=fake_resolution, verdict=fake_verdict)
    assert meta["cross_model_vendor"] == "codex"
    assert meta["cross_model_family"] == "openai"
    assert meta["cross_model_degraded"] == "true"
    assert meta["cross_model_verdict"] == "pass"


def test_build_metadata_records_skipped_chain_for_audit():
    """Operator must be able to see which entries were skipped and why (the
    auditor/CXDB expects a comma-joined string identical to
    `_DEFAULT_ADVERSARIAL_PRIORITY`'s metadata shape)."""
    cm = _load_cross_model_module()

    fake_resolution = cm.CrossModelResolution(
        vendor="agy",
        family="google",
        skipped=("cursor-agent", "codex"),
        degraded=False,
    )
    fake_verdict = cm.CrossModelVerdict(
        raw_verdict="pass",
        normalized_outcome="success",
        parsed_text="verdict: pass",
    )
    meta = cm.build_metadata(resolution=fake_resolution, verdict=fake_verdict)
    assert meta["cross_model_skipped"] == "cursor-agent,codex"


# ---------------------------------------------------------------------------
# Vendor-down fallback test (acceptance: vendor-down fallback test in spec)
# ---------------------------------------------------------------------------


def test_vendor_down_fallback_returns_resolution_with_skipped_chain(monkeypatch):
    """Per the acceptance criteria: a vendor-down scenario must still produce
    a usable assessment — either a different vendor, or a degraded result.
    The resolver NEVER returns an empty `(vendor=None, family=None)` when at
    least one chain entry is installed."""
    cm = _load_cross_model_module()

    # cursor-agent "down" — its binary is on PATH but exits non-zero on
    # --version (the cheap probe returns False on rc!=0).
    def fake_probe(name):
        return name == "codex"

    resolution = cm.resolve_cross_model_reviewer(
        coder_family="anthropic", probe_fn=fake_probe
    )
    assert resolution.vendor == "codex", "vendor-down cursor must NOT block the chain"
    assert "cursor-agent" in resolution.skipped


# ---------------------------------------------------------------------------
# Unknown / empty chain entries fail closed
# ---------------------------------------------------------------------------


def test_unknown_vendor_in_chain_rejected(monkeypatch):
    """A vendor that is not on the canonical chain must be rejected at
    resolution time. We do NOT silently invent a backend."""
    cm = _load_cross_model_module()

    with_pyiteration = cm.CROSS_MODEL_VENDOR_CHAIN + ("made-up-vendor",)
    with pytest.raises(ValueError, match="made-up-vendor"):

        cm.resolve_cross_model_reviewer(
            coder_family="anthropic",
            chain=with_pyiteration,
            probe_fn=lambda name: True,
        )


# ---------------------------------------------------------------------------
# Acceptance telemetry: two model families on a normal run
# ---------------------------------------------------------------------------


def test_normal_run_has_two_families(monkeypatch):
    """Acceptance: 'assessment telemetry shows two model families on a normal
    run'. A normal run = coder anthropic + cross_model cursor / codex / agy.
    """
    cm = _load_cross_model_module()

    def fake_probe(name):
        return name == "cursor-agent"

    resolution = cm.resolve_cross_model_reviewer(
        coder_family="anthropic", probe_fn=fake_probe
    )
    coder_family = "anthropic"
    cross_family = resolution.family
    families = {coder_family, cross_family}
    assert len(families) == 2, (
        f"normal run must show two distinct model families; got {families}"
    )
    assert resolution.degraded is False


# ---------------------------------------------------------------------------
# TYPE_REGISTRY contract: ``gate_cross_model`` must be registered so a
# .dot node carrying ``type="gate_cross_model"`` routes to the handler
# rather than falling back to _codergen (silent regression hazard).
# ---------------------------------------------------------------------------


def test_gate_cross_model_is_registered_in_type_registry():
    """Defensive: a graph that authors
    ``gate_cross_model [type="gate_cross_model"]`` must route to the
    handler — falling through to _codergen would be a silent regression
    (cf. tests/test_graph_audit.py::test_g3_gate_skeptic_unregistered_fails
    that locks down the same contract for gate_skeptic).
    """
    from runner import handlers
    assert "gate_cross_model" in handlers.TYPE_REGISTRY, (
        "gate_cross_model missing from TYPE_REGISTRY; check "
        "runner/handlers.py for the registration"
    )


def test_gate_cross_model_echo_backend_emits_metadata():
    """When run under the echo backend (test/CI path), the gate must
    emit ``cross_model_vendor`` / ``cross_model_degraded`` /
    ``cross_model_verdict`` into ``Result.metadata`` so operators and
    CXDB readers see the cross-model invariant."""
    from runner.handler_universal_prompts import _gate_cross_model
    from runner.handler_core import Context
    from runner.parser import Node

    sentinel_node = Node(
        name="gate_cross_model",
        attrs={"type": "gate_cross_model"},
    )
    sentinel_ctx = Context(
        goal="test",
        workdir=pathlib.Path("/tmp"),
        backend="echo",
        state={
            "gate_cross_model.outcome": "success",
        },
    )

    result = _gate_cross_model(sentinel_node, sentinel_ctx)
    assert result.outcome == "success", (
        f"echo backend success hint should pass; got {result.outcome!r}"
    )
    assert "cross_model_vendor" in result.metadata, (
        "metadata missing cross_model_vendor key; strict-merge #328 needs it"
    )
    assert "cross_model_degraded" in result.metadata, (
        "metadata missing cross_model_degraded key; strict-merge #328 needs it"
    )
    assert "cross_model_verdict" in result.metadata
    assert result.metadata["cross_model_vendor"] != "(none)", (
        "echo backend must still emit a sensible vendor for audit trail"
    )
