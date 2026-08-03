"""Per-backend CLI-fallback coverage — Lane B PR (test/cli-fallback-coverage).

This file locks in the *current* missing-CLI behavior of every backend
(`claude`, `codex`, `agy`, `ao`) in two dispatch paths:

  1. The **coder** path — ``_codergen`` in ``runner/handlers.py``.
  2. The **reviewer-gate** path — ``_execute_gate`` / ``_run_gate_once``,
     reached via the priority queue (``backend_priority=...``) and the
     claude infra-fallback chain.

The audit doc (``docs/cli-fallback-audit-2026-06-12.md``) is the
companion document. Each test name references the audit table row it
pins.

Lane discipline: this file does NOT modify ``runner/handlers.py`` (WIP'd
by the ``claudeaf`` author on Lane A) and does NOT touch
``runner/__main__.py`` (WIP). Tests document the current behavior
honestly — including the one confirmed panic (agy Popen, bead
jleechan-c5q) — rather than papering over it.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner.handlers import (  # noqa: E402
    Context,
    _codergen,
    _execute_gate,
    _resolve_gate_backend,
    _run_gate_once,
)
from runner.parser import Node as _Node  # noqa: E402

# The audit's tl;dr table — these are the outcomes we lock in. Any
# regression here (e.g. agy starts catching FileNotFoundError after the
# jleechan-c5q fix) is a deliberate contract change and should land in
# a separate PR with the xfail updated to a regular test.
EXPECTED_CODER_OUTCOME = {
    "claude": "failure",    # caught by `except Exception` (line 535)
    "codex": "error",       # caught by `except Exception` (line 583)
    "agy": "error",         # Popen is protected (bead jleechan-c5q)
    "ao": "error",          # terminal transport failure
}

EXPECTED_GATE_BACKEND_MISSING_OUTCOME = {
    # These backends ship as their own CLI on argv[0]. Missing CLI →
    # FileNotFoundError → backend_missing="true", outcome="error".
    "claude": ("error", "true"),
    "codex": ("error", "true"),
    "agy": ("error", "true"),
    # ``minimax`` and ``claudeaf`` are *claude-routed* — they invoke
    # the claude CLI binary with different env. The argv[0] is still
    # "claude", so a missing-CLI FileNotFoundError happens at the
    # subprocess level, but the recorded ``reviewer_backend`` reflects
    # the *logical* backend (minimax/claudeaf), not the physical
    # binary. They share the gate's ``FileNotFoundError`` handler.
    "minimax": ("error", "true"),
    "claudeaf": ("error", "true"),
}


# ---------------------------------------------------------------------------
# Helpers — sandbox-disable shims, fake binaries, completed-process stubs
# ---------------------------------------------------------------------------


def _node(backend: str, name: str = "test_step") -> _Node:
    """Build a codergen node pinned to ``backend``."""
    return _Node(name=name, attrs={"type": "codergen", "backend": backend})


def _disable_sandbox(monkeypatch) -> None:
    """Make ``_sandboxed_args`` and ``_sandboxed_args_for_workdir`` transparent passthroughs so the tests
    focus on the *backend* argv, not the sandbox wrapper."""
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: list(a))
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda a, w: list(a))


def _fake_completed(args, *, returncode: int = 0, stdout: str = "", stderr: str = ""):
    """Subprocess.CompletedProcess-like stand-in."""
    return subprocess.CompletedProcess(args, returncode, stdout=stdout, stderr=stderr)


def test_codex_worker_timeout_with_byte_diagnostics_is_terminal_error(tmp_path, monkeypatch):
    """TimeoutExpired may carry bytes despite ``text=True``; never crash decoding it."""
    from runner.subprocess_control import BoundedProcessResult

    _disable_sandbox(monkeypatch)

    def _timeout(*args, **kwargs):
        return BoundedProcessResult(
            args=("codex",),
            returncode=-15,
            stdout="partial stdout\n",
            stderr="partial stderr\n",
            timed_out=True,
        )

    monkeypatch.setattr("runner.handler_codergen.run_bounded_process", _timeout)
    result = _codergen(
        _Node(
            name="worker",
            attrs={"type": "codergen", "backend": "codex", "timeout": "1"},
        ),
        Context(goal="test timeout", workdir=tmp_path, backend="codex"),
    )

    assert result.outcome == "error"
    assert result.metadata["timed_out"] == "true"
    assert "partial stdout" in result.output
    assert "partial stderr" in result.output


def _first_arg_matches(args, target: str) -> bool:
    """True when ``args[0]`` resolves to ``target`` by basename OR by
    substring match (so a faked ``/nonexistent/claude-binary-for-test``
    still matches ``target="claude"``)."""
    first = os.path.basename(str(args[0]))
    return first == target or target in str(args[0])


def _patched_run_raises(monkeypatch, *, target: str, exc: BaseException):
    """Make ``subprocess.run`` raise ``exc`` when ``args[0]`` matches ``target``,
    otherwise return a benign completed-process stub.

    Used to simulate a missing CLI without actually removing the binary from
    PATH (which would be fragile and cross-platform). The match is by basename
    OR by substring so faked paths like ``/nonexistent/<target>-binary-for-test``
    still work.
    """
    def _fake_run(args, **kwargs):
        if _first_arg_matches(args, target):
            raise exc
        return _fake_completed(args, returncode=0, stdout="", stderr="")

    def _fake_bounded(args, **kwargs):
        from runner.subprocess_control import BoundedProcessResult

        completed = _fake_run(args, **kwargs)
        return BoundedProcessResult(
            args=tuple(str(part) for part in args),
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
            timed_out=False,
        )

    monkeypatch.setattr("runner.handlers.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_codergen.subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_codergen.run_bounded_process", _fake_bounded)
    monkeypatch.setattr("runner.handler_dispatch.run_bounded_process", _fake_bounded)
    monkeypatch.setattr("subprocess.run", _fake_run)


def _patched_popen_raises(monkeypatch, *, target: str, exc: BaseException):
    """Make ``subprocess.Popen`` raise ``exc`` when invoked with ``target`` as
    the first argv entry. Used to simulate the agy-panic bug (Popen is
    unprotected in the coder path). Match is by basename OR by substring.
    """
    real_popen = subprocess.Popen

    def _fake_popen(args, **kwargs):
        if _first_arg_matches(args, target):
            raise exc
        return real_popen(args, **kwargs)

    monkeypatch.setattr("runner.handlers.subprocess.Popen", _fake_popen)
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _fake_popen)
    monkeypatch.setattr("subprocess.Popen", _fake_popen)


# ---------------------------------------------------------------------------
# Coder path — missing CLI per backend
# ---------------------------------------------------------------------------


def test_claude_coder_missing_returns_clean_failure(monkeypatch, tmp_path):
    """claude missing → ``Result(outcome="failure", ...)`` via the
    ``except Exception as e:`` at line 535 of ``_codergen``. No panic.

    Audit-table row: claude | failure | YES | NO (gate path is a
    separate test).
    """
    _disable_sandbox(monkeypatch)
    # claude executable is normally resolved by PATH; force a known-bad path
    # so the test is hermetic regardless of the host environment.
    monkeypatch.setattr(
        "runner.handlers._get_claude_executable",
        lambda: "/nonexistent/claude-binary-for-test",
    )
    _patched_run_raises(
        monkeypatch,
        target="claude",
        exc=FileNotFoundError(2, "No such file or directory: claude"),
    )

    result = _codergen(_node("claude"), Context(goal="t", workdir=tmp_path, backend="claude"))

    assert result.outcome == "failure", (
        f"expected clean failure on missing claude, got {result.outcome!r}: {result.output!r}"
    )
    assert "claude backend error" in result.output
    # coder path does NOT set backend_missing (only the gate path does).
    assert result.metadata.get("backend_missing") != "true"


def test_codex_coder_missing_returns_clean_error(monkeypatch, tmp_path):
    """codex missing → ``Result(outcome="error", ...)`` via the
    ``except Exception as exc:`` at line 583.

    Audit-table row: codex | error | YES | NO.

    The codex branch's convention is ``outcome="error"`` (not
    ``"failure"``) on a missing binary. This test pins that asymmetry.
    """
    _disable_sandbox(monkeypatch)
    _patched_run_raises(
        monkeypatch,
        target="codex",
        exc=FileNotFoundError(2, "No such file or directory: codex"),
    )

    result = _codergen(_node("codex"), Context(goal="t", workdir=tmp_path, backend="codex"))

    assert result.outcome == "error", (
        f"expected outcome='error' on missing codex, got {result.outcome!r}: {result.output!r}"
    )
    assert "codex backend error" in result.output
    assert result.metadata.get("backend_missing") != "true"


def test_codex_missing_fixture_cannot_launch_real_process(monkeypatch, tmp_path):
    _disable_sandbox(monkeypatch)
    _patched_run_raises(
        monkeypatch,
        target="codex",
        exc=FileNotFoundError(2, "No such file or directory: codex"),
    )

    def _forbid_popen(*args, **kwargs):
        raise AssertionError("test fixture attempted a real subprocess launch")

    monkeypatch.setattr("runner.subprocess_control.subprocess.Popen", _forbid_popen)
    result = _codergen(
        _node("codex"),
        Context(goal="t", workdir=tmp_path, backend="codex"),
    )

    assert result.outcome == "error"
    assert "codex backend error" in result.output


def test_agy_coder_missing_panics_unprotected(monkeypatch, tmp_path):
    """agy missing → ``FileNotFoundError`` is caught and returned as clean error Result.
    """
    _disable_sandbox(monkeypatch)
    _patched_popen_raises(
        monkeypatch,
        target="agy",
        exc=FileNotFoundError(2, "No such file or directory: agy"),
    )

    result = _codergen(_node("agy"), Context(goal="t", workdir=tmp_path, backend="agy"))
    assert result.outcome == "error"
    assert result.metadata.get("backend_missing") == "true"



def test_ao_coder_missing_sandbox_present_returns_failure(monkeypatch, tmp_path):
    """ao missing (sandbox-exec present) → terminal ``Result(outcome="error", ...)``
    via the ``except Exception as exc:`` at line 352 (``ao spawn`` branch).

    Audit-table row: ao | failure | YES (via Exception) | NO.
    """
    _disable_sandbox(monkeypatch)
    # Pre-seed ``ao.session`` so we go down the ``ao send`` path? No —
    # the spawn path is the one most commonly hit in a fresh run. The
    # send path has the same ``except Exception`` (line 445), so this
    # test covers both via the spawn branch. We also exercise the send
    # branch in the missing-sandbox test below.
    _patched_run_raises(
        monkeypatch,
        target="ao",
        exc=FileNotFoundError(2, "No such file or directory: ao"),
    )
    monkeypatch.setattr("runner.handlers._ao_wait_idle", lambda *a, **kw: "ready")

    ctx = Context(goal="t", workdir=tmp_path, backend="ao")
    ctx.state["ao.project"] = "fake-project"

    result = _codergen(_node("ao", name="ao_spawn"), ctx)

    assert result.outcome == "error", (
        f"expected error on missing ao, got {result.outcome!r}: {result.output!r}"
    )
    assert "ao spawn failed" in result.output
    assert result.metadata.get("backend_missing") != "true"


def test_ao_coder_missing_sandbox_explicitly_unavailable_returns_failure(monkeypatch, tmp_path):
    """``_sandboxed_args`` returns ``None`` when ``sandbox-exec`` is absent,
    and both ``ao spawn`` (line 325) and ``ao send`` (line 419) short-circuit
    to a clean ``Result(outcome="failure", output="sandbox-exec unavailable")``.

    This is the second leg of the audit-table row for ``ao``: if
    ``sandbox-exec`` is missing, the binary is never even invoked, but
    the handler still returns a clean ``failure`` rather than panicking.
    """
    # Override _sandboxed_args to return None (mimicking the
    # sandbox-exec-missing branch).
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: None)

    ctx = Context(goal="t", workdir=tmp_path, backend="ao")
    ctx.state["ao.project"] = "fake-project"

    result = _codergen(_node("ao", name="ao_spawn"), ctx)

    assert result.outcome == "failure"
    assert "sandbox-exec unavailable" in result.output


# ---------------------------------------------------------------------------
# Reviewer-gate path — FileNotFoundError handler (line 1198)
# ---------------------------------------------------------------------------


def test_gate_per_backend_missing_sets_backend_missing_metadata(monkeypatch, tmp_path):
    """``_run_gate_once`` has a dedicated ``except FileNotFoundError`` at
    line 1198 that sets ``metadata["backend_missing"] = "true"`` and
    returns ``outcome="error"``.

    Parametrize over every reviewer-gate backend so the audit table's
    `metadata["backend_missing"]="true"` cell stays accurate.

    Implementation note: ``minimax`` and ``claudeaf`` are claude-routed
    -- the physical argv[0] is the claude binary. So we patch
    ``_get_claude_executable`` to return a path that substring-matches
    "claude" but raises on the claude CLI invocation. The recorded
    ``reviewer_backend`` still reflects the logical backend name
    (minimax / claudeaf) -- that's the end-to-end dispatch guarantee.
    """
    fake_sha = "0" * 40
    for backend, (expected_outcome, expected_flag) in EXPECTED_GATE_BACKEND_MISSING_OUTCOME.items():
        if backend in ("claude", "minimax", "claudeaf"):
            # claude-routed: physical argv[0] is the claude binary. The
            # path must contain "claude" so _first_arg_matches fires on
            # the right target.
            monkeypatch.setattr(
                "runner.handlers._get_claude_executable",
                lambda b=backend: f"/nonexistent/claude-for-{b}",
            )
            physical_target = f"claude-for-{backend}"
        else:
            physical_target = backend

        def _fake_run(cmd, _target=physical_target, **kwargs):
            if _first_arg_matches(cmd, _target):
                raise FileNotFoundError(2, f"No such file or directory: {_target}")
            return _fake_completed(
                cmd, returncode=0,
                stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="",
            )

        monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: list(a))
        monkeypatch.setattr("subprocess.run", _fake_run)
        monkeypatch.setattr("runner.handler_dispatch.run_bounded_process", _fake_run)
        monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)

        ctx = Context(goal="t", workdir=tmp_path, backend="claude")
        result = _run_gate_once(backend, "PROMPT", fake_sha, 300, ctx, "gate_x")

        # The first-attempt result has backend_missing="true" (which is
        # why _execute_gate triggers the fallback). We assert directly on
        # _run_gate_once to avoid the fallback's verdict masking the
        # diagnostic flag.
        assert result.outcome == expected_outcome, (
            f"backend={backend!r}: expected outcome={expected_outcome!r}, "
            f"got {result.outcome!r}: {result.output!r}"
        )
        assert result.metadata.get("backend_missing") == expected_flag, (
            f"backend={backend!r}: expected backend_missing={expected_flag!r}, "
            f"got {result.metadata.get('backend_missing')!r}; metadata={result.metadata!r}"
        )
        assert result.metadata.get("reviewer_backend") == backend
        # No SHA echo because the subprocess never ran.
        assert result.metadata.get("head_sha_status") == "missing"


def test_reviewer_gate_priority_queue_with_codex_missing_picks_codex(monkeypatch, tmp_path):
    """``_resolve_adversarial_backend`` picks the first entry that
    responds to ``--version`` (probe stubbed here). When the resolved
    backend is then missing at run time, ``_execute_gate`` walks the
    infra-failure fallback chain in order: ``agy`` first (if not already
    the resolved backend), then ``claude``. The chain
    "codex (missing) -> agy (missing) -> claude (succeeds)" is the
    canonical path now that agy→claude infra fallback was added (commit
    981e26bd9).

    Note: this test deliberately fails BOTH codex and agy in the stub so
    the fallback walks all the way to claude. The companion test
    ``test_reviewer_gate_priority_queue_agy_succeeds_after_codex_missing``
    covers the "agy succeeds" mid-chain case.
    """
    fake_sha = "0" * 40
    seen: list[str] = []

    # Stub the resolver probe so codex is "installed" (returns True) and
    # the resolver picks it as the first entry.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name == "codex",
    )

    def _fake_run(cmd, **kwargs):
        first = os.path.basename(str(cmd[0]))
        seen.append(first)
        if first in ("codex", "agy"):
            # codex: probe said installed but binary missing at run time.
            # agy: also simulated missing to force walk to claude.
            raise FileNotFoundError(2, f"No such file or directory: {first}")
        # claude fallback: succeed with a real verdict + SHA echo.
        return _fake_completed(
            cmd, returncode=0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: list(a))
    monkeypatch.setattr("subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.run_bounded_process", _fake_run)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)

    node = _Node(
        name="ev",
        attrs={"backend_priority": "codex,minimax,agy,claude-sonnet"},
    )
    ctx = Context(goal="t", workdir=tmp_path, backend="claude")

    resolved, meta = _resolve_gate_backend(node, ctx)
    assert resolved == "codex", (
        f"priority queue should pick codex (first probeable entry) on "
        f"first call, got {resolved!r}; meta={meta!r}"
    )
    assert meta["adversarial_resolved"] == "codex"
    assert meta["adversarial_skipped"] == ""

    # Run the gate. codex raises FileNotFoundError -> claude fallback
    # fires (recorded in metadata) and succeeds with a real verdict.
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "ev", resolved)

    assert result.outcome == "success"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "codex"
    assert result.metadata["reviewer_backend"] == "claude"
    # Confirm the dispatch order: codex (FileNotFoundError) -> agy (also
    # missing) -> claude (succeeds). The agy→claude fallback walks every
    # entry until one is not an infra failure (handler_dispatch.py:_execute_gate).
    assert seen == ["codex", "agy", "claude"]


def test_reviewer_gate_priority_queue_all_uninstalled_falls_through(monkeypatch, tmp_path):
    """When *no* entry in the priority queue is installed, the resolver
    falls through to the LAST entry (per
    ``_resolve_adversarial_backend`` lines 1346-1347). The gate then
    runs that last entry, which raises FileNotFoundError, and the
    claude fallback engages.

    The metadata records the full skip list so the operator can see
    which backends were attempted via probe but not at run time.

    Implementation note: we use a non-claude-routed last entry (``agy``)
    because ``_execute_gate`` short-circuits the fallback when the
    resolved backend is ``claude`` / ``claude-sonnet`` (it IS the
    fallback). The audit's "claude-sonnet falls through" path is
    exercised in the test_gate_per_backend_missing test.
    """
    fake_sha = "0" * 40
    seen: list[str] = []

    # All entries report as not installed.
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: False,
    )

    def _fake_run(cmd, **kwargs):
        first = os.path.basename(str(cmd[0]))
        seen.append(first)
        if first == "agy":
            raise FileNotFoundError(2, "No such file or directory: agy")
        return _fake_completed(
            cmd, returncode=0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: list(a))
    monkeypatch.setattr("subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.run_bounded_process", _fake_run)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)

    # Use a non-claude-routed last entry (agy) so the fallback fires.
    node = _Node(
        name="ev",
        attrs={"backend_priority": "codex,minimax,agy"},
    )
    ctx = Context(goal="t", workdir=tmp_path, backend="claude")

    resolved, meta = _resolve_gate_backend(node, ctx)
    # Last entry in the queue, regardless of probe.
    assert resolved == "agy"
    # All three entries show up in the skip list.
    assert meta["adversarial_skipped"] == "codex,minimax,agy"

    # agy is missing -> claude fallback fires.
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "ev", resolved)
    assert result.outcome == "success"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "agy"
    assert result.metadata["reviewer_backend"] == "claude"
    assert seen == ["agy", "claude"]


def test_reviewer_gate_agy_missing_falls_back_to_claude_cleanly(monkeypatch, tmp_path):
    """agy is the reviewer, FileNotFoundError, ``backend_missing="true"``
    in the first-attempt result, then claude fallback succeeds. This is
    the canonical "agy missing → claude" path the gate infrastructure
    was built to support.
    """
    fake_sha = "0" * 40
    seen: list[str] = []

    def _fake_run(cmd, **kwargs):
        first = os.path.basename(str(cmd[0]))
        seen.append(first)
        if first == "agy":
            raise FileNotFoundError(2, "No such file or directory: agy")
        return _fake_completed(
            cmd, returncode=0,
            stdout=f"head_sha: {fake_sha}\nverdict: pass\n", stderr="",
        )

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: list(a))
    monkeypatch.setattr("subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.run_bounded_process", _fake_run)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)

    ctx = Context(goal="t", workdir=tmp_path, backend="claude")
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "ev", "agy")

    assert result.outcome == "success"
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "agy"
    assert result.metadata["reviewer_backend"] == "claude"
    assert seen == ["agy", "claude"]


def test_reviewer_gate_all_backends_missing_tags_infra_failure(monkeypatch, tmp_path):
    """Every entry in the priority queue is missing AND the claude
    fallback is also missing → ``verdict="infra_failure"`` so the
    operator can distinguish "no reviewer ever graded the diff" from a
    real FAIL. This is the contract
    ``_execute_gate`` documents at lines 1453-1455.
    """
    fake_sha = "0" * 40
    seen: list[str] = []

    def _fake_run(cmd, **kwargs):
        first = os.path.basename(str(cmd[0]))
        seen.append(first)
        raise FileNotFoundError(2, f"No such file or directory: {first}")

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda a: list(a))
    monkeypatch.setattr("subprocess.run", _fake_run)
    monkeypatch.setattr("runner.handler_dispatch.run_bounded_process", _fake_run)
    monkeypatch.setattr("runner.handlers._worktree_head_sha", lambda p: fake_sha)

    # Use a single-backend explicit case so we don't depend on probe
    # behavior — the priority-queue probe is a separate concern (see
    # test_adversarial_priority_picks_first_installed in test_gates.py).
    ctx = Context(goal="t", workdir=tmp_path, backend="claude")
    result = _execute_gate("PROMPT", fake_sha, 300, ctx, "ev", "codex")

    # Both codex and the claude fallback raised FileNotFoundError.
    assert result.outcome == "error"
    assert result.metadata["verdict"] == "infra_failure", (
        f"expected verdict='infra_failure' when both codex and claude "
        f"fallback are missing, got {result.metadata!r}"
    )
    assert result.metadata["fallback_used"] == "true"
    assert result.metadata["fallback_from"] == "codex"
    assert result.metadata["reviewer_backend"] == "claude"
    assert result.metadata["backend_missing"] == "true", (
        "the FINAL result should still surface backend_missing='true' from "
        "the claude fallback attempt (it's an infra failure, after all)"
    )
    # Both backends were attempted.
    assert "codex" in seen
    assert "claude" in seen


def test_target_worktree_ao_worktree_review_binding(tmp_path):
    """AO worker writes to `ao.worktree`; target worktree resolver must bind
    `_target_worktree(ctx)` to `ao.worktree` so controller review snapshots the
    worker's target tree instead of an untouched `ctx.workdir`."""
    from runner.handlers import _target_worktree

    main_dir = tmp_path / "main_repo"
    main_dir.mkdir()
    ao_wt_dir = tmp_path / "ao_worktree"
    ao_wt_dir.mkdir()

    ctx = Context(goal="test ao worktree binding", workdir=main_dir)
    assert _target_worktree(ctx) == main_dir.resolve()

    ctx.state["ao.worktree"] = str(ao_wt_dir)
    assert _target_worktree(ctx) == ao_wt_dir.resolve()


def test_controller_review_codex_unavailable_fails_closed(monkeypatch, tmp_path):
    """Controller review has no non-Codex fallback transport to advertise."""
    from runner.handlers import Result, _execute_gate, _resolve_gate_backend

    _disable_sandbox(monkeypatch)
    monkeypatch.setattr(
        "runner.handlers._probe_backend_installed",
        lambda name: name in ("agy", "minimax", "claude-sonnet"),
    )

    node = _Node(
        name="cold_reviewer",
        attrs={
            "review_contract": "cold-review-v1",
            "backend_priority": "codex",
        },
    )
    ctx = Context(goal="test fallback", workdir=tmp_path, backend="claude")
    resolved, meta = _resolve_gate_backend(node, ctx)

    assert resolved == "codex"
    assert meta["adversarial_priority"] == "codex"

    attempts: list[str] = []

    def _missing_controller_transport(backend, *args, **kwargs):
        attempts.append(backend)
        return Result(
            outcome="error",
            output="codex unavailable",
            metadata={"backend_missing": "true", "verdict": "unknown"},
        )

    monkeypatch.setattr(
        "runner.handler_dispatch._run_gate_once", _missing_controller_transport
    )
    ctx.state["_df_controller_review_json"] = "true"
    result = _execute_gate("PROMPT", "0" * 40, 300, ctx, "cold_reviewer", "codex")

    assert result.outcome == "error"
    assert result.metadata["verdict"] == "infra_failure"
    assert attempts == ["codex"]


def test_controller_codex_transport_preserves_outer_sandbox_exec():
    """Controller Codex stays inside the sealed-path-denying outer sandbox."""
    from runner.handler_dispatch import _build_controller_codex_transport

    sandboxed_argv = [
        "/usr/bin/sandbox-exec",
        "-p",
        '(version 1)\n(deny file-read* (subpath "/sealed/dark-factory-holdouts"))',
        "/usr/local/bin/codex",
        "exec",
        "--yolo",
        "--skip-git-repo-check",
        "prompt text",
    ]
    reviewed_source = pathlib.Path("/reviewed/source")
    transport = _build_controller_codex_transport(
        sandboxed_argv,
        read_only_paths=(reviewed_source,),
    )

    assert transport[:2] == sandboxed_argv[:2]
    assert "dark-factory-holdouts" in transport[2]
    assert f'(deny file-write* (subpath "{reviewed_source}"))' in transport[2]
    assert transport[3] == "/usr/local/bin/codex"
    assert transport[4] == "exec"
    assert "--sandbox" in transport
    assert "danger-full-access" in transport


def test_controller_outer_sandbox_enforces_read_and_write_boundaries(tmp_path):
    """One outer wrapper denies sealed reads and immutable-target writes."""
    from runner.handler_dispatch import _build_controller_codex_transport

    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        pytest.skip("sandbox-exec unavailable")

    denied_dir = tmp_path / "synthetic-denied"
    denied_dir.mkdir()
    denied_file = denied_dir / "secret.txt"
    denied_file.write_text("synthetic-secret\n", encoding="utf-8")
    allowed_file = tmp_path / "allowed.txt"
    allowed_file.write_text("allowed-marker\n", encoding="utf-8")
    source_dir = tmp_path / "reviewed-source"
    source_dir.mkdir()
    snapshot_dir = tmp_path / "reviewed-snapshot"
    snapshot_dir.mkdir()
    fake_codex = tmp_path / "codex"
    fake_codex.write_text(
        "#!/bin/sh\n"
        'if /bin/cat "$DF_SYNTHETIC_DENIED" >/dev/null 2>/dev/null; then\n'
        "  exit 90\n"
        "fi\n"
        'if printf changed >"$DF_REVIEWED_SOURCE/write.txt" 2>/dev/null; then\n'
        "  exit 92\n"
        "fi\n"
        'if printf changed >"$DF_REVIEWED_SNAPSHOT/write.txt" 2>/dev/null; then\n'
        "  exit 93\n"
        "fi\n"
        '/bin/cat "$DF_SYNTHETIC_ALLOWED" || exit 91\n'
        "printf 'controller-invoked\\n'\n",
        encoding="utf-8",
    )
    fake_codex.chmod(0o755)
    profile = (
        "(version 1)\n"
        "(allow default)\n"
        f'(deny file-read* (subpath "{denied_dir}"))'
    )
    transport = _build_controller_codex_transport(
        [sandbox_exec, "-p", profile, str(fake_codex), "exec", "prompt"],
        read_only_paths=(source_dir, snapshot_dir),
    )

    proc = subprocess.run(
        transport,
        input="bounded synthetic probe",
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "DF_SYNTHETIC_DENIED": str(denied_file),
            "DF_SYNTHETIC_ALLOWED": str(allowed_file),
            "DF_REVIEWED_SOURCE": str(source_dir),
            "DF_REVIEWED_SNAPSHOT": str(snapshot_dir),
        },
        timeout=10,
        check=False,
    )

    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.splitlines() == ["allowed-marker", "controller-invoked"]
    assert "synthetic-secret" not in proc.stdout
    assert not (source_dir / "write.txt").exists()
    assert not (snapshot_dir / "write.txt").exists()
    assert "--sandbox" in transport and "danger-full-access" in transport
    assert "--dangerously-bypass-approvals-and-sandbox" not in transport


def test_implementing_agent_sandbox_denies_runner_venv_writes(tmp_path, monkeypatch):
    if os.environ.get("DARK_FACTORY_OUTER_SANDBOX") == "1":
        pytest.skip("already executing under the outer implementing-agent sandbox")
    from runner import handler_sandbox

    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        pytest.skip("sandbox-exec unavailable")
    holdouts = tmp_path / "holdouts"
    holdouts.mkdir()
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts))
    venv = tmp_path / ".venv"
    venv.mkdir()
    ordinary = tmp_path / "ordinary.txt"
    args = handler_sandbox._sandboxed_args_for_workdir(
        ["/bin/sh", "-c", 'printf bad >.venv/injected; printf good >ordinary.txt'],
        tmp_path,
    )
    assert args is not None

    proc = subprocess.run(args, cwd=tmp_path, capture_output=True, text=True, check=False)

    assert not (venv / "injected").exists()
    assert ordinary.read_text() == "good"


def test_implementing_agent_sandbox_denies_controller_private_trust_operations(
    tmp_path, monkeypatch
):
    if os.environ.get("DARK_FACTORY_OUTER_SANDBOX") == "1":
        trust_root = pathlib.Path.home() / ".dark-factory" / "operator-trust"
        probe = trust_root / f"worker-probe-{os.getpid()}"
        proc = subprocess.run(
            [
                "/bin/sh", "-c",
                'test ! -r "$DF_TRUST_ROOT/registry.json" || exit 81; '
                '/bin/ls "$DF_TRUST_ROOT" >/dev/null 2>&1 && exit 82; '
                'printf bad >"$DF_PROBE" 2>/dev/null && exit 83; exit 0',
            ],
            capture_output=True,
            text=True,
            env={
                **os.environ,
                "DF_TRUST_ROOT": str(trust_root),
                "DF_PROBE": str(probe),
            },
            check=False,
        )
        assert proc.returncode == 0, proc.stderr
        assert not probe.exists()
        return
    from runner import handler_sandbox

    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        pytest.skip("sandbox-exec unavailable")
    holdouts = tmp_path / "holdouts"
    holdouts.mkdir()
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(holdouts))
    controller_root = tmp_path / "controller-private"
    trust_root = controller_root / "operator-trust"
    trust_root.mkdir(parents=True)
    trust_file = trust_root / "registry.json"
    trust_file.write_text("sealed-trust\n", encoding="utf-8")
    outside = tmp_path / "outside.txt"
    outside.write_text("outside\n", encoding="utf-8")
    monkeypatch.setattr(
        handler_sandbox, "_controller_private_root", lambda: trust_root
    )
    probe = (
        'test ! -e "$DF_TRUST_ROOT/registry.json" || exit 81; '
        'test ! -r "$DF_TRUST_ROOT/registry.json" || exit 82; '
        'test "$(/bin/ls "$DF_TRUST_ROOT" 2>/dev/null)" = "" || exit 83; '
        'printf bad >"$DF_TRUST_ROOT/injected" 2>/dev/null && exit 84; '
        '/bin/mv "$DF_OUTSIDE" "$DF_TRUST_ROOT/moved" 2>/dev/null && exit 85; '
        '/bin/mv "$DF_TRUST_FILE" "$DF_OUTSIDE.moved" 2>/dev/null && exit 86; '
        '/bin/rm "$DF_TRUST_FILE" 2>/dev/null && exit 87; '
        'printf allowed >"$DF_ALLOWED"'
    )
    args = handler_sandbox._sandboxed_args_for_workdir(
        ["/bin/sh", "-c", probe], tmp_path
    )
    assert args is not None

    proc = subprocess.run(
        args,
        cwd=tmp_path,
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "DF_CONTROLLER_ROOT": str(controller_root),
            "DF_TRUST_ROOT": str(trust_root),
            "DF_TRUST_FILE": str(trust_file),
            "DF_OUTSIDE": str(outside),
            "DF_ALLOWED": str(tmp_path / "allowed.txt"),
        },
        check=False,
    )

    assert proc.returncode == 0, proc.stderr
    assert trust_file.read_text(encoding="utf-8") == "sealed-trust\n"
    assert outside.read_text(encoding="utf-8") == "outside\n"
    assert not (trust_root / "injected").exists()
    assert (tmp_path / "allowed.txt").read_text(encoding="utf-8") == "allowed"


def test_controller_outer_sandbox_avoids_nested_seatbelt(tmp_path):
    """Outer Seatbelt remains authoritative without unsupported nesting."""
    from runner.handler_dispatch import _build_controller_codex_transport

    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        pytest.skip("sandbox-exec unavailable")

    denied_dir = tmp_path / "synthetic-denied"
    denied_dir.mkdir()
    fake_codex = tmp_path / "codex"
    fake_codex.write_text(
        "#!/bin/sh\n"
        'case " $* " in\n'
        '  *" --sandbox read-only "*)\n'
        '    exec "$DF_SANDBOX_EXEC" -p "(version 1)(allow default)" /usr/bin/true\n'
        "    ;;\n"
        '  *" --sandbox danger-full-access "*)\n'
        "    exec /usr/bin/true\n"
        "    ;;\n"
        "  *) exit 92 ;;\n"
        "esac\n",
        encoding="utf-8",
    )
    fake_codex.chmod(0o755)
    profile = (
        "(version 1)\n"
        "(allow default)\n"
        f'(deny file-read* (subpath "{denied_dir}"))'
    )
    transport = _build_controller_codex_transport(
        [sandbox_exec, "-p", profile, str(fake_codex), "exec", "prompt"],
        read_only_paths=(tmp_path / "reviewed-source",),
    )

    proc = subprocess.run(
        transport,
        input="bounded nested-seatbelt probe",
        capture_output=True,
        text=True,
        env={**os.environ, "DF_SANDBOX_EXEC": sandbox_exec},
        timeout=10,
        check=False,
    )

    assert "--sandbox" in transport and "danger-full-access" in transport
    assert proc.returncode == 0, proc.stderr
    assert "sandbox_apply" not in proc.stderr


def test_controller_protects_linked_git_metadata_and_artifact_lane(tmp_path):
    """The outer boundary owns linked Git metadata and controller artifacts."""
    from runner.handler_dispatch import (
        _build_controller_codex_transport,
        _controller_protected_paths,
    )

    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        pytest.skip("sandbox-exec unavailable")

    source = tmp_path / "source"
    source.mkdir()
    subprocess.run(["git", "-C", str(source), "init", "-q"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(source),
            "config",
            "user.email",
            "jleechan2015@users.noreply.github.com",
        ],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(source), "config", "user.name", "test"],
        check=True,
    )
    (source / "tracked.txt").write_text("base\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(source), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(source), "commit", "-qm", "base"], check=True)
    reviewed = tmp_path / "linked-review"
    subprocess.run(
        ["git", "-C", str(source), "worktree", "add", "--detach", str(reviewed), "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    )
    git_dir = pathlib.Path(
        subprocess.run(
            ["git", "-C", str(reviewed), "rev-parse", "--git-dir"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    ).resolve()
    common_dir = pathlib.Path(
        subprocess.run(
            ["git", "-C", str(reviewed), "rev-parse", "--git-common-dir"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    ).resolve()
    output_dir = tmp_path / "controller-artifacts"
    output_dir.mkdir()
    allowed_file = tmp_path / "allowed.txt"
    allowed_file.write_text("allowed-marker\n", encoding="utf-8")
    fake_codex = tmp_path / "codex"
    fake_codex.write_text(
        "#!/bin/sh\n"
        'if printf changed >"$DF_GIT_DIR/reviewer-write" 2>/dev/null; then exit 90; fi\n'
        'if printf changed >"$DF_COMMON_DIR/reviewer-write" 2>/dev/null; then exit 91; fi\n'
        'if printf changed >"$DF_OUTPUT/reviewer-write" 2>/dev/null; then exit 92; fi\n'
        'if /bin/ln -s "$DF_ALLOWED" "$DF_OUTPUT/reviewer-link" 2>/dev/null; then exit 93; fi\n'
        '/bin/cat "$DF_GIT_DIR/HEAD" >/dev/null || exit 94\n'
        '/bin/cat "$DF_ALLOWED"\n',
        encoding="utf-8",
    )
    fake_codex.chmod(0o755)
    protected = _controller_protected_paths((reviewed,), output_dir=output_dir)
    transport = _build_controller_codex_transport(
        [
            sandbox_exec,
            "-p",
            "(version 1)\n(allow default)",
            str(fake_codex),
            "exec",
            "prompt",
        ],
        read_only_paths=protected,
    )

    assert git_dir in protected
    assert common_dir in protected
    assert output_dir.resolve() in protected
    proc = subprocess.run(
        transport,
        input="bounded linked-worktree probe",
        capture_output=True,
        text=True,
        env={
            **os.environ,
            "DF_GIT_DIR": str(git_dir),
            "DF_COMMON_DIR": str(common_dir),
            "DF_OUTPUT": str(output_dir),
            "DF_ALLOWED": str(allowed_file),
        },
        timeout=10,
        check=False,
    )

    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == "allowed-marker\n"
    assert not (git_dir / "reviewer-write").exists()
    assert not (common_dir / "reviewer-write").exists()
    assert not (output_dir / "reviewer-write").exists()
    assert not (output_dir / "reviewer-link").exists()

    external = tmp_path / "external-replacement"
    external.mkdir()
    root_probe = tmp_path / "codex-root-probe" / "codex"
    root_probe.parent.mkdir()
    root_probe.write_text(
        "#!/bin/sh\n"
        'if [ "$DF_MODE" = rename ]; then\n'
        '  if /bin/mv "$DF_TARGET" "$DF_MOVED" 2>/dev/null; then\n'
        "    printf 'root-rename-bypass\\n'\n"
        "    exit 90\n"
        "  fi\n"
        "  printf 'root-rename-denied\\n'\n"
        "  exit 0\n"
        "fi\n"
        'if /bin/ln -s "$DF_EXTERNAL" "$DF_TARGET" 2>/dev/null; then\n'
        "  printf 'root-replacement-bypass\\n'\n"
        "  exit 91\n"
        "fi\n"
        "printf 'root-replacement-denied\\n'\n",
        encoding="utf-8",
    )
    root_probe.chmod(0o755)
    root_transport = _build_controller_codex_transport(
        [
            sandbox_exec,
            "-p",
            "(version 1)\n(allow default)",
            str(root_probe),
            "exec",
            "prompt",
        ],
        read_only_paths=protected,
    )
    root_results = {}
    for label, target in (
        ("output", output_dir),
        ("source", reviewed),
        ("git_dir", git_dir),
        ("git_common_dir", common_dir),
    ):
        moved = target.with_name(target.name + "-reviewer-moved")
        rename_result = subprocess.run(
            root_transport,
            input=f"bounded {label} root-rename probe",
            capture_output=True,
            text=True,
            env={
                **os.environ,
                "DF_MODE": "rename",
                "DF_TARGET": str(target),
                "DF_MOVED": str(moved),
                "DF_EXTERNAL": str(external),
            },
            timeout=10,
            check=False,
        )
        if moved.exists() and not target.exists():
            moved.rename(target)

        target.rename(moved)
        replacement_result = subprocess.run(
            root_transport,
            input=f"bounded {label} root-replacement probe",
            capture_output=True,
            text=True,
            env={
                **os.environ,
                "DF_MODE": "replace",
                "DF_TARGET": str(target),
                "DF_MOVED": str(moved),
                "DF_EXTERNAL": str(external),
            },
            timeout=10,
            check=False,
        )
        if target.is_symlink():
            target.unlink()
        moved.rename(target)
        root_results[label] = (rename_result, replacement_result)

    assert {
        label: tuple((result.returncode, result.stdout.strip()) for result in results)
        for label, results in root_results.items()
    } == {
        label: (
            (0, "root-rename-denied"),
            (0, "root-replacement-denied"),
        )
        for label in ("output", "source", "git_dir", "git_common_dir")
    }
    assert all(not path.is_symlink() for path in (output_dir, reviewed, git_dir, common_dir))
    (output_dir / "controller-receipt.json").write_text("accepted\n")
    assert (output_dir / "controller-receipt.json").read_text() == "accepted\n"


def test_controller_protected_paths_deduplicate_ordinary_repo_metadata(tmp_path):
    """An ordinary repository with one Git/common dir yields unique real paths."""
    from runner.handler_dispatch import _controller_protected_paths

    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
    output_dir = tmp_path / "output"

    protected = _controller_protected_paths((repo,), output_dir=output_dir)

    assert protected.count((repo / ".git").resolve()) == 1
    assert len(protected) == len(set(protected))
