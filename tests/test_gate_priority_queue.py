"""Adversarial-review priority queue: codex > minimax > agy > claude-sonnet.

The queue is the FIRST adversarial pass selector — *not* a retry cascade. A
real fail|partial from the chosen backend is kept (no-reviewer-shopping
rule, feedback_2026-05-31_runner_resilience_reviewer_gates.md).

Extracted from tests/test_gates.py per docs/refactor/file-ownership-map.test_gates.md.
"""
from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402


def _priority_node(priority, *, prefer_adversarial=False, name="evidence"):
    attrs = {"backend_priority": ",".join(priority)}
    if prefer_adversarial:
        attrs["prefer_adversarial"] = "true"
    return make_node(name=name, **attrs)


def test_adversarial_priority_picks_first_installed(monkeypatch):
    """When the head of the priority list is installed, it is chosen."""
    from runner.handlers import (
        _resolve_adversarial_backend,
        _resolve_gate_backend,
        Context as HCtx,
    )

    node = _priority_node(["definitely-not-installed-aaa", "codex"])
    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")

    # `codex` is the only entry we expect to probe-true. Stub the probe so
    # the test is hermetic and doesn't depend on what's on PATH right now.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "codex",
    )

    resolved, meta = _resolve_adversarial_backend(
        ["definitely-not-installed-aaa", "codex"], ctx
    )
    assert resolved == "codex"
    assert meta["adversarial_resolved"] == "codex"
    assert meta["adversarial_skipped"] == "definitely-not-installed-aaa"

    # The gate resolver also returns the priority-queue audit metadata.
    backend, gate_meta = _resolve_gate_backend(node, ctx)
    assert backend == "codex"
    assert gate_meta["adversarial_resolved"] == "codex"
    assert gate_meta["reviewer_backend_resolution"] == "priority_queue"
    assert gate_meta["prefer_adversarial"] == "false"


def test_adversarial_priority_demotes_coder_backend_when_prefer_adversarial(monkeypatch):
    """prefer_adversarial: true DEMOTES the run-level coder backend to last
    rather than dropping it. Any other installed entry still wins, so a
    `claude` coder run gets a `claude` reviewer only when nothing else is
    available -- see the companion test below."""
    from runner.handlers import (
        _resolve_adversarial_backend,
        _resolve_gate_backend,
        Context as HCtx,
    )

    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")

    # `claude` is installed (the coder), `codex` is installed, `agy` is not.
    # The queue is `[claude, codex, agy]`. With prefer_adversarial the
    # `claude` entry should be dropped, then `codex` should win.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name in ("claude", "codex"),
    )

    resolved, meta = _resolve_adversarial_backend(
        ["claude", "codex", "agy"], ctx
    )
    # `claude` was the coder, prefer_adversarial drops it; the resolver
    # was given the *post-filter* queue (the gate resolver applies the
    # filter before calling _resolve_adversarial_backend). Verify the
    # gate-level filter is the one that drops the coder backend.
    assert resolved == "claude"  # the resolver picks from what it got
    assert meta["adversarial_resolved"] == "claude"

    # The gate-level resolver, however, must apply prefer_adversarial BEFORE
    # calling the priority resolver.
    node = _priority_node(
        ["claude", "codex", "agy"], prefer_adversarial=True, name="evidence"
    )
    backend, gate_meta = _resolve_gate_backend(node, ctx)
    assert backend == "codex", (
        f"prefer_adversarial must demote the coder backend; got {backend!r}"
    )
    ordered = gate_meta["adversarial_priority"].split(",")
    assert "claude" in ordered, (
        f"the coder backend must be demoted, not removed; got {ordered!r}"
    )
    assert ordered.index("claude") > ordered.index("codex"), (
        f"the coder backend must rank below the other lane entries; got {ordered!r}"
    )
    assert gate_meta["prefer_adversarial"] == "true"


def test_prefer_adversarial_reviews_on_coder_backend_when_it_is_the_only_option(
    monkeypatch,
):
    """Demotion, not exclusion: when the coder's backend is the only installed
    entry, the lane reviews on it instead of escaping to a vendor nobody put
    in the queue. The old hard filter emptied the list here and fell through
    to the default priority, which reached for codex first -- the expensive
    default this change exists to avoid.
    """
    from runner.handlers import _resolve_gate_backend, Context as HCtx

    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="minimax")
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "minimax",
    )

    node = _priority_node(
        ["minimax", "codex"], prefer_adversarial=True, name="evidence"
    )
    backend, meta = _resolve_gate_backend(node, ctx)

    assert backend == "minimax", (
        f"the only installed backend must be used, not skipped; got {backend!r}"
    )
    assert "codex" in meta["adversarial_skipped"].split(","), (
        "codex must have been probed and skipped as uninstalled, "
        f"got skipped={meta['adversarial_skipped']!r}"
    )


def test_adversarial_priority_env_override_honored(monkeypatch):
    """DARK_FACTORY_ADVERSARIAL_PRIORITY env var overrides the default queue."""
    from runner.handlers import _resolve_adversarial_backend, Context as HCtx

    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")
    monkeypatch.setenv("DARK_FACTORY_ADVERSARIAL_PRIORITY", "minimax,codex,agy")
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name in ("codex", "agy"),  # minimax is NOT installed
    )

    resolved, meta = _resolve_adversarial_backend(None, ctx)
    assert resolved == "codex", (
        f"with minimax uninstalled and codex installed, the resolver must "
        f"fall through to codex; got {resolved!r}"
    )
    assert meta["adversarial_priority"] == "minimax,codex,agy"
    assert meta["adversarial_skipped"] == "minimax"


def test_adversarial_priority_falls_through_to_claude_sonnet_when_nothing_else(monkeypatch):
    """When no priority entry is installed, the resolver returns the last
    entry (claude-sonnet) so the gate still runs and surfaces the missing
    binary honestly. The gate's backend_missing=true path is the real
    signal that nothing was installed — the resolver does not silently
    downgrade or shop reviewers."""
    from runner.handlers import _resolve_adversarial_backend, Context as HCtx

    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")
    monkeypatch.delenv("DARK_FACTORY_ADVERSARIAL_PRIORITY", raising=False)
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: False,  # nothing on PATH
    )

    resolved, meta = _resolve_adversarial_backend(None, ctx)
    # claude-sonnet is the tail of the default queue; the resolver falls
    # through to it so the gate can run and the missing-binary path fires.
    assert resolved == "claude-sonnet", (
        f"with nothing installed, resolver must fall through to claude-sonnet; got {resolved!r}"
    )
    # The full default queue is recorded as skipped — operator can see
    # why the gate is running on a tail-end entry. The order is
    # probe-then-fallthrough, so the tail entry is also marked skipped
    # (the gate's missing-binary path is the real signal).
    assert meta["adversarial_skipped"] == "codex,minimax,agy,claude-sonnet"
    assert meta["adversarial_priority"] == "codex,minimax,agy,claude-sonnet"


def test_adversarial_priority_pinned_across_visits(monkeypatch):
    """Cross-visit pin: once `_resolve_gate_backend` resolves a node via the
    priority queue, re-visits to the same node name return the same backend
    even when `_probe_backend_installed` would resolve differently. This
    honors the design-doc promise in
    `roadmap/agy-reviewer-and-base-dot-2026-06-09.md` §5.2 ("the runner
    pins the reviewer for the entire run") and the no-reviewer-shopping
    rule (a real fail from one backend is never re-resolved onto a
    different one on a re-visit). Regression test for the verifier's
    Concern 1 in `agy-task-review.md`."""
    from runner.handlers import _resolve_gate_backend, Context as HCtx

    node = _priority_node(["codex", "minimax", "agy", "claude-sonnet"], name="evidence")
    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="claude")

    # First visit: `codex` is the only entry installed → it is the
    # chosen backend. The pin is recorded in ctx.state.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "codex",
    )
    first, first_meta = _resolve_gate_backend(node, ctx)
    assert first == "codex"
    assert first_meta["reviewer_backend_resolution"] == "priority_queue"
    assert ctx.state["evidence.resolved_backend"] == "codex"

    # Now `codex` disappears from PATH (e.g. uninstalled mid-run).
    # Only `agy` is installed. Without the cross-visit pin, the
    # resolver would fall through to `agy` — a different vendor, a
    # different verdict. With the pin, the second visit returns the
    # *same* backend (`codex`) and the same metadata, honoring the
    # "pinned for the entire run" promise.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "agy",
    )
    second, second_meta = _resolve_gate_backend(node, ctx)
    assert second == "codex", (
        f"cross-visit pin broken: re-visit must return pinned backend, got {second!r}"
    )
    assert second_meta["reviewer_backend_resolution"] == "priority_queue"


def test_probe_backend_installed_maps_claude_sonnet_to_claude() -> None:
    """_probe_backend_installed("claude-sonnet") must probe the standard `claude` binary."""
    from runner.handlers import _probe_backend_installed
    # When `claude` is installed on PATH (as in this environment), probing
    # "claude-sonnet" must return True.
    import shutil
    if shutil.which("claude"):
        assert _probe_backend_installed("claude-sonnet") is True


def test_adversarial_priority_falls_back_to_claude_when_others_unavailable(monkeypatch) -> None:
    """When codex, minimax, agy are unavailable, reviewer priority falls back to claude-sonnet (which probes claude)."""
    from runner.handlers import (
        _resolve_adversarial_backend,
        Context as HCtx,
    )

    ctx = HCtx(goal="test", workdir=pathlib.Path("/tmp"), backend="ao")
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "claude-sonnet",
    )

    resolved, meta = _resolve_adversarial_backend(
        ["codex", "minimax", "agy", "claude-sonnet"], ctx
    )
    assert resolved == "claude-sonnet"
    assert meta["adversarial_resolved"] == "claude-sonnet"
    assert meta["adversarial_skipped"] == "codex,minimax,agy"
