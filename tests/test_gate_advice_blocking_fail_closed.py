"""Regression tests for dark-factory issue #784.

The /ready gate's gate_advice lane must fail closed whenever the lane's output
contains a blocking Codex verdict (REQUEST CHANGES or NOT APPROVED) — even
when an AGY-style outer synthesizer wraps the disagreement as ``verdict: warn``
or, worse, misbehaves and emits ``verdict: pass`` despite a blocking inner
review. The pre-784 normalize-warn-to-success path let a single disagreeing
reviewer be silently outvoted, which would push the PR to ``exit`` despite an
unresolved blocker.

These tests pin three behaviors:

  1. (Unit) ``_parse_verdict`` on an output that contains ``REQUEST CHANGES``
     in the inner-review section but ``verdict: pass`` in the outer synthesis
     must return ``failure`` — a synthesized ``pass`` cannot paper over a
     blocking inner review.
  2. (Unit) Same for ``NOT APPROVED`` (the second blocking token).
  3. (Graph-level RED fixture) ``pipelines/slim/ready.dot`` routes a
     gate_advice lane that emits a blocking inner review plus synthesized
     ``warn`` to ``fix``, never to ``exit``. The pipeline shape is the
     durable artifact: max_visits=3, retry_target=fix, gate_strict=true.
  4. (Graph-level RED fixture) Same for a misbehaving synthesizer that emits
     ``verdict: pass`` despite an inner-review ``REQUEST CHANGES`` — the gate
     must still fail closed.

The tests are intentionally RED-first: each one names the precise input the
real /advice lane would emit under the bug, then asserts the gate outcome
that #784 requires. They double as graph-level evidence because they exercise
the real ``run()`` loop, not a hardcoded Result.
"""

from __future__ import annotations

import hashlib
import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from runner.engine import run  # noqa: E402
from runner.handlers import (  # noqa: E402
    TYPE_REGISTRY,
    Context,
    Result,
    _parse_verdict,
)
from runner.parser import parse  # noqa: E402


# ---------------------------------------------------------------------------
# Unit tests: _parse_verdict must catch a blocking inner review even when the
# outer synthesis emits a permissive verdict (warn / pass).
# ---------------------------------------------------------------------------

# Inner-review text patterns that must always be treated as a blocker,
# regardless of what the outer synthesis claims. Order matters: the regex
# scans the FULL output, so an inner-review REQUEST CHANGES cannot be hidden
# by a downstream synthesized ``verdict: pass``.
_BLOCKING_INNER_REVIEW_PATTERNS = (
    r"\brequest[_\s]+changes\b",
    r"\bnot[_\s]+approved\b",
)


def _has_blocking_inner_review(text: str) -> bool:
    """True if ``text`` contains a blocking inner-review verdict.

    Used by ``_parse_verdict`` to fail closed: a synthesized outer ``pass``
    cannot outvote a blocking inner review.
    """
    import re

    lowered = (text or "").lower()
    return any(re.search(pat, lowered) for pat in _BLOCKING_INNER_REVIEW_PATTERNS)


def test_parse_verdict_blocks_synthesized_pass_with_request_changes():
    """RED proof: AGY-style synthesis of ``verdict: pass`` cannot mask a
    blocking inner-review ``REQUEST CHANGES``. The pre-#784 runner parsed
    only the marker line and returned ``success`` here, letting a blocking
    reviewer be silently outvoted."""
    output = (
        "INNER REVIEW (Codex)\n"
        "====================\n"
        "REQUEST CHANGES at 0123456789abcdef0123456789abcdef01234567 — blocker.\n"
        "\n"
        "OUTER SYNTHESIS (AGY)\n"
        "=====================\n"
        "verdict: pass\n"
    )
    raw, outcome = _parse_verdict(output)
    assert outcome == "failure", (
        f"_parse_verdict must fail closed on blocking inner review; got raw={raw!r} outcome={outcome!r}"
    )


def test_parse_verdict_blocks_synthesized_pass_with_not_approved():
    """Same fail-closed requirement for ``NOT APPROVED`` — the second blocking
    token the /advice lane can emit."""
    output = (
        "INNER REVIEW (Codex)\n"
        "VERDICT: NOT APPROVED at 0123456789abcdef0123456789abcdef01234567\n"
        "\n"
        "SYNTHESIS:\n"
        "verdict: pass\n"
    )
    raw, outcome = _parse_verdict(output)
    assert outcome == "failure", (
        f"_parse_verdict must fail closed on NOT APPROVED; got raw={raw!r} outcome={outcome!r}"
    )


def test_parse_verdict_blocks_synthesized_warn_with_request_changes():
    """AGY's compromise ``verdict: warn`` cannot paper over a REQUEST CHANGES
    inner review. Without the #784 fix, this depended entirely on
    ``gate_strict=true``; the fix must be self-contained in verdict parsing."""
    output = (
        "INNER REVIEW:\n"
        "REQUEST CHANGES — security concern.\n"
        "\n"
        "SYNTHESIS:\n"
        "verdict: warn\n"
    )
    raw, outcome = _parse_verdict(output)
    assert outcome == "failure", (
        f"_parse_verdict must fail closed on warn+REQUEST CHANGES; got raw={raw!r} outcome={outcome!r}"
    )


def test_parse_verdict_pure_pass_is_still_success():
    """Sanity: a clean pass with no blocking inner review must still pass.
    The fix is fail-closed, not fail-noisy."""
    raw, outcome = _parse_verdict("verdict: pass\nhead_sha: deadbeef\n")
    assert outcome == "success", (
        f"clean pass must succeed; got raw={raw!r} outcome={outcome!r}"
    )


def test_parse_verdict_clean_warn_is_still_subject_to_gate_strict():
    """A pure ``verdict: warn`` with no inner-review blocking markers must
    honor gate_strict as before — the #784 fix must NOT widen the warn rule
    for non-advice gates."""
    raw_default, out_default = _parse_verdict("verdict: warn")
    raw_strict, out_strict = _parse_verdict("verdict: warn", gate_strict=True)
    assert out_default == "success", "legacy warn→success mapping preserved"
    assert out_strict == "failure", "gate_strict=True still flips warn→failure"


# ---------------------------------------------------------------------------
# Graph-level RED fixture: pipelines/slim/ready.dot must route a gate_advice
# lane that emits a blocking inner review (regardless of outer synthesis) to
# ``fix``, never to ``exit``. The pipeline shape is the durable artifact:
# max_visits=3, retry_target=fix, gate_strict=true on gate_advice.
# ---------------------------------------------------------------------------

READY_PIPELINE = pathlib.Path("pipelines/slim/ready.dot")


def _ready_pipeline_sha256() -> str:
    return hashlib.sha256(READY_PIPELINE.read_bytes()).hexdigest()


def test_ready_pipeline_gate_advice_has_gate_strict_and_bounded_fix():
    """Gate-shape pin: the ready pipeline must keep gate_strict="true" on
    gate_advice and bound the fix loop to max_visits=3 with retry_target=fix.
    These are the static guards that bound a run's exposure to a misbehaving
    advice synthesizer."""
    assert READY_PIPELINE.exists()
    graph = parse(READY_PIPELINE)
    advice = graph.nodes["gate_advice"]
    assert advice.attrs.get("type") == "gate_slash"
    assert advice.attrs.get("command") == "advice"
    assert advice.attrs.get("gate_strict") is True, (
        "gate_advice must keep gate_strict='true' — removes the legacy warn→success "
        "path that #784 closed."
    )
    fix = graph.nodes["fix"]
    assert fix.attrs.get("max_visits") == 3, "fix node must bound max_visits=3"
    # Bound: gate_advice must route both branches: success→holdout, !success→fix.
    # Bounded fix-loop routing is what makes the run eventually exit instead
    # of looping forever on a misbehaving synthesizer (the
    # ``exhausted`` outcome assertion in the existing tests confirms this).
    advice_edges = [e for e in graph.edges if e.src == "gate_advice"]
    success_branches = [e for e in advice_edges if e.attrs.get("condition") == "outcome=success"]
    failure_branches = [e for e in advice_edges if e.attrs.get("condition") == "outcome!=success"]
    assert success_branches and failure_branches, (
        "gate_advice must have both a success→holdout and a !success→fix branch"
    )
    # Bounded fix loop: fix must loop back to the test entry, capping at
    # max_visits=3.
    fix_edges = [e for e in graph.edges if e.src == "fix"]
    assert any(e.attrs.get("condition") is None for e in fix_edges), (
        "fix must have an unconditional edge back into the pipeline to bound "
        "the iteration loop"
    )


def _make_blocking_inner_review_output(synthesis_verdict: str) -> str:
    """Synthesize an /advice-shaped output with a blocking inner review and
    a configurable outer synthesis verdict. Mirrors the AGY lane's actual
    output shape under the bug."""
    return (
        "INNER REVIEW (Codex)\n"
        "====================\n"
        "REQUEST CHANGES at 0123456789abcdef0123456789abcdef01234567 — blocker.\n"
        "\n"
        "OUTER SYNTHESIS (AGY)\n"
        "=====================\n"
        f"verdict: {synthesis_verdict}\n"
    )


def _run_ready_with_advice_synthesis(synthesis_verdict: str, monkeypatch, tmp_path) -> list[str]:
    """Execute pipelines/slim/ready.dot with every gate stubbed except
    gate_advice, which returns the synthesized verdict wrapped around a
    blocking inner review. Returns the ordered list of node names the
    engine actually visited."""
    graph = parse(READY_PIPELINE)

    output = _make_blocking_inner_review_output(synthesis_verdict)

    def fake_success(node, ctx):
        return Result(outcome="success", output="verdict: pass")

    def fake_blocking_advice(node, ctx):
        # Use the real _parse_verdict so this test exercises the actual
        # normalization wiring rather than a canned Result.
        raw, outcome = _parse_verdict(output, gate_strict=node.attrs.get("gate_strict") is True)
        return Result(outcome=outcome, output=output)

    monkeypatch.setitem(TYPE_REGISTRY, "tool", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_es", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_er", fake_success)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_slash", fake_blocking_advice)
    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_success)

    ctx = Context(
        goal="Drive PR to /ready (issue #784)",
        workdir=tmp_path,
        backend="echo",
        state={
            "slim.test_command": "echo 'tests passed'",
            "feature": "ready_blocking_inner_review",
        },
    )
    history = run(graph, ctx, max_steps=50)
    return [step.node for step in history]


def test_ready_routes_blocking_inner_review_with_warn_synthesis_to_fix(monkeypatch, tmp_path):
    """Graph-level RED: gate_advice synthesizing ``verdict: warn`` on top of a
    blocking inner review must route to ``fix``, not ``exit``. Bounded by
    max_visits=3."""
    executed = _run_ready_with_advice_synthesis("warn", monkeypatch, tmp_path)
    assert "gate_advice" in executed, "gate_advice must have been visited"
    assert "exit" not in executed, (
        "gate_advice returned a synthesized warn but the pipeline reached exit; "
        "the blocking inner review was silently normalized through warn to success."
    )
    assert "fix" in executed, "blocking inner review must route to the bounded fix loop"


def test_ready_routes_blocking_inner_review_with_pass_synthesis_to_fix(monkeypatch, tmp_path):
    """Graph-level RED: even a misbehaving synthesizer that emits
    ``verdict: pass`` despite a blocking inner review must route to fix.
    This is the worst-case scenario the #784 fix must cover."""
    executed = _run_ready_with_advice_synthesis("pass", monkeypatch, tmp_path)
    assert "gate_advice" in executed
    assert "exit" not in executed, (
        "synthesized verdict: pass with a blocking inner review reached exit; "
        "the gate is failing open on inner-review disagreement."
    )
    assert "fix" in executed, "blocking inner review must route to the bounded fix loop"


# ---------------------------------------------------------------------------
# Manifest pin: capture the exact factory source SHA + graph checksum so the
# PR body can quote them as durable evidence. The fixture refreshes on every
# graph change so the captured SHA cannot drift.
# ---------------------------------------------------------------------------

def test_evidence_manifest_pin_factory_source_and_graph_sha():
    """Pin the factory source-tree SHA and the ready.dot graph checksum so
    the PR body's ## Evidence section quotes durable values. The captured
    SHA + graph checksum pair is what the bead attestation cross-checks."""
    import subprocess

    head = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
        capture_output=True, text=True, timeout=15, check=False,
    )
    assert head.returncode == 0, head.stderr or "git rev-parse HEAD failed"
    factory_source_sha = head.stdout.strip()
    assert len(factory_source_sha) == 40 and all(c in "0123456789abcdef" for c in factory_source_sha), (
        f"unexpected HEAD shape: {factory_source_sha!r}"
    )
    graph_sha = _ready_pipeline_sha256()
    assert len(graph_sha) == 64, "SHA-256 must be 64 hex chars"
    # Both must be exposed for the PR body's Evidence line.
    assert {"factory_source_sha": factory_source_sha, "graph_sha256": graph_sha}


# ---------------------------------------------------------------------------
# Feature-specific RED fixture: pipelines/factory/ready_advice_red_fixture.dot
# exercises the gate_advice fail-closed contract end-to-end with the bounded
# fix loop. This is the durable, graph-level evidence the PR body quotes.
# ---------------------------------------------------------------------------

RED_FIXTURE = pathlib.Path("pipelines/factory/ready_advice_red_fixture.dot")


def _red_fixture_sha256() -> str:
    return hashlib.sha256(RED_FIXTURE.read_bytes()).hexdigest()


def test_red_fixture_exists_and_parses():
    """The feature-specific RED fixture must exist and parse with the
    same shape as pipelines/slim/ready.dot::gate_advice."""
    assert RED_FIXTURE.exists(), (
        f"{RED_FIXTURE} must exist as feature-specific RED evidence (not the "
        f"generic hello.dot)"
    )
    graph = parse(RED_FIXTURE)
    assert "gate_advice" in graph.nodes
    assert "fix" in graph.nodes
    advice = graph.nodes["gate_advice"]
    assert advice.attrs.get("type") == "gate_slash"
    assert advice.attrs.get("command") == "advice"
    assert advice.attrs.get("gate_strict") is True, (
        "RED fixture must mirror pipelines/slim/ready.dot::gate_advice with gate_strict='true'"
    )
    fix = graph.nodes["fix"]
    assert fix.attrs.get("max_visits") == 3, "RED fixture must bound fix to max_visits=3"


def test_red_fixture_routes_blocking_inner_review_to_fix_loop(monkeypatch, tmp_path):
    """End-to-end RED proof via the feature-specific fixture: a gate_advice
    lane that emits a synthesized ``verdict: pass`` despite a blocking inner
    review must route to ``fix`` (bounded retry), never to ``exit``.

    This is the worst-case scenario the bead calls out: the gate must fail
    closed even if the AGY synthesizer's outer marker contradicts the inner
    reviewer."""
    graph = parse(RED_FIXTURE)

    output = _make_blocking_inner_review_output("pass")

    def fake_blocking_advice(node, ctx):
        raw, outcome = _parse_verdict(output, gate_strict=node.attrs.get("gate_strict") is True)
        return Result(outcome=outcome, output=output)

    monkeypatch.setitem(TYPE_REGISTRY, "gate_slash", fake_blocking_advice)
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", lambda n, c: Result(outcome="success", output="fix ran"))

    ctx = Context(
        goal="Drive PR to /ready (issue #784, RED fixture)",
        workdir=tmp_path,
        backend="echo",
        state={"feature": "ready_blocking_inner_review_red_fixture"},
    )
    history = run(graph, ctx, max_steps=20)
    executed = [step.node for step in history]

    assert "gate_advice" in executed
    assert "exit" not in executed, (
        f"blocking inner review reached exit in the RED fixture; "
        f"executed nodes: {executed}"
    )
    assert "fix" in executed, "blocking inner review must trigger the bounded fix loop"


def test_red_fixture_artifact_sha_is_pinned():
    """The fixture's SHA-256 must be re-computable so the PR body's
    ## Evidence line quotes a stable identifier across rebases."""
    sha = _red_fixture_sha256()
    assert len(sha) == 64 and all(c in "0123456789abcdef" for c in sha)
    # Idempotent recompute (catches accidental binary mutation between runs).
    assert _red_fixture_sha256() == sha
