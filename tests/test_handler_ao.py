"""Tests for `runner.handler_ao` — AO status polling helpers.

These tests lock in two correctness/robustness properties of
``_ao_wait_idle`` and ``_ao_parse_status``:

  1. ``_ao_wait_idle`` MUST honour its ``project`` argument and pass
     ``-p <project>`` to ``ao status`` for faster fleet-scoped queries.
  2. ``_ao_wait_idle`` MUST NOT propagate ``subprocess.TimeoutExpired``
     from the inner ``ao status`` probe; the helper should treat the
     probe as a transient failure and keep polling until the outer
     deadline elapses, then return ``"timeout"``.

A third test guards ``_ao_parse_status`` against an empty/whitespace-only
status payload so the handler degrades to ``"unknown"`` rather than raising.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys
from unittest import mock

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

# Import via the canonical re-export entry point to avoid the circular
# import triggered by importing handler_ao directly (handlers.py re-exports
# these via its own top-level import path).
from runner.handlers import _ao_parse_status, _ao_wait_idle  # noqa: E402

# ---------------------------------------------------------------------------
# _ao_parse_status — defensive parsing
# ---------------------------------------------------------------------------


def test_parse_status_empty_stdout_returns_unknown():
    """Empty ``ao status`` output (or just notifier noise) must degrade to
    ``"unknown"`` rather than raise."""
    assert _ao_parse_status("", "any-session") == "unknown"


def test_parse_status_whitespace_only_returns_unknown():
    """Whitespace/preamble-only output (no JSON array) must degrade to
    ``"unknown"``."""
    assert _ao_parse_status("   \n\n   \n", "any-session") == "unknown"


def test_parse_status_malformed_json_returns_unknown():
    """A truncated or malformed JSON array must degrade to ``"unknown"``,
    not raise ``json.JSONDecodeError``."""
    assert _ao_parse_status("[{'name': 'x', 'activity':", "x") == "unknown"


def test_parse_status_session_present_returns_activity():
    assert _ao_parse_status('[{"name": "S1", "activity": "ready"}]', "S1") == "ready"


def test_parse_status_session_missing_returns_missing():
    assert (
        _ao_parse_status('[{"name": "OTHER", "activity": "active"}]', "S1") == "missing"
    )


def test_parse_status_strips_preamble_before_first_bracket():
    """`ao status` prepends notifier noise lines; the parser must skip
    everything before the first ``[``."""
    payload = (
        "INFO: notifier plugin initialised\n"
        "WARN: deprecated flag\n"
        '[{"name": "S1", "activity": "exited"}]'
    )
    assert _ao_parse_status(payload, "S1") == "exited"


# ---------------------------------------------------------------------------
# _ao_wait_idle — must honour `project` argument
# ---------------------------------------------------------------------------


def test_wait_idle_passes_project_filter_to_ao_status(tmp_path, monkeypatch):
    """When ``project`` is provided, the helper must invoke
    ``ao status -p <project> --json`` so the query is filtered to that
    project (matters on large fleets)."""
    captured: dict[str, list[str]] = {}
    call_count = {"n": 0}

    class _FakeCompleted:
        def __init__(self) -> None:
            self.returncode = 0
            self.stdout = '[{"name": "S1", "activity": "exited"}]'
            self.stderr = ""

        # The handler reads these attributes directly.
        returncode = 0
        stdout = '[{"name": "S1", "activity": "exited"}]'
        stderr = ""

    def fake_run(args, **kwargs):
        call_count["n"] += 1
        captured["args"] = list(args)
        # Return "exited" on the first probe so the helper returns early.
        completed = mock.Mock(returncode=0)
        completed.stdout = '[{"name": "S1", "activity": "exited"}]'
        completed.stderr = ""
        return completed

    monkeypatch.setattr("runner.handler_ao.subprocess.run", fake_run)

    activity = _ao_wait_idle(
        "S1",
        tmp_path,
        timeout=30,
        stable_reads=3,
        poll_interval=0,
        project="my-project",
    )

    assert activity == "exited"
    args = captured["args"]
    assert "-p" in args, f"expected -p flag in argv, got {args!r}"
    p_idx = args.index("-p")
    assert args[p_idx + 1] == "my-project", f"expected -p my-project, got {args!r}"


def test_wait_idle_without_project_omits_filter(tmp_path, monkeypatch):
    """When ``project`` is None, the helper must NOT pass a stray
    ``-p`` / ``None`` pair to ``ao status``."""
    captured: dict[str, list[str]] = {}

    def fake_run(args, **kwargs):
        captured["args"] = list(args)
        completed = mock.Mock(returncode=0)
        completed.stdout = '[{"name": "S1", "activity": "exited"}]'
        completed.stderr = ""
        return completed

    monkeypatch.setattr("runner.handler_ao.subprocess.run", fake_run)

    _ao_wait_idle("S1", tmp_path, timeout=30, poll_interval=0, project=None)

    args = captured["args"]
    assert "-p" not in args, f"unexpected -p flag in argv when project=None: {args!r}"


# ---------------------------------------------------------------------------
# _ao_wait_idle — must not propagate subprocess.TimeoutExpired
# ---------------------------------------------------------------------------


def test_wait_idle_does_not_propagate_status_timeout(tmp_path, monkeypatch):
    """If the inner ``ao status --json`` probe hangs past its 180s
    subprocess timeout, the helper must NOT let ``TimeoutExpired``
    propagate. Instead it should keep polling until the outer deadline
    elapses and return ``"timeout"``.

    We simulate by raising ``TimeoutExpired`` on the first probe and
    returning "exited" on the second probe, with a very tight outer
    deadline — the helper must consume the timeout AND keep going.
    """
    probes: list[list[str]] = []

    def fake_run(args, **kwargs):
        probes.append(list(args))
        if len(probes) == 1:
            raise subprocess.TimeoutExpired(cmd=args, timeout=180)
        # Second probe: declare exited.
        completed = mock.Mock(returncode=0)
        completed.stdout = '[{"name": "S1", "activity": "exited"}]'
        completed.stderr = ""
        return completed

    monkeypatch.setattr("runner.handler_ao.subprocess.run", fake_run)

    activity = _ao_wait_idle(
        "S1", tmp_path, timeout=5, stable_reads=3, poll_interval=0, project=None
    )

    # Must NOT have raised; should have retried and seen "exited".
    assert (
        activity == "exited"
    ), f"helper should recover from inner TimeoutExpired, got {activity!r}"
    assert (
        len(probes) >= 2
    ), f"helper should retry after TimeoutExpired, got {len(probes)} probes"


def test_wait_idle_returns_timeout_when_probe_always_times_out(tmp_path, monkeypatch):
    """If every probe times out, the helper must return ``"timeout"``
    cleanly without raising — the outer deadline is the budget."""

    def fake_run(args, **kwargs):
        raise subprocess.TimeoutExpired(cmd=args, timeout=180)

    monkeypatch.setattr("runner.handler_ao.subprocess.run", fake_run)

    activity = _ao_wait_idle(
        "S1", tmp_path, timeout=2, stable_reads=3, poll_interval=0, project=None
    )

    assert (
        activity == "timeout"
    ), f"expected timeout when probes always TimeoutExpired, got {activity!r}"
