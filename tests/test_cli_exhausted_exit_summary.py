"""Regression tests for bead rev-oswpo: non-zero exit + stderr summary on
exhausted dispatch.

ROOT-CAUSE: `dark-factory` already returns a non-zero exit code when the
final history outcome isn't "success" (see `runner/__main__.py::main`
`return 0 if history and history[-1].outcome == "success" else 1`), but it
never writes anything to stderr — the failure is only visible by parsing
the stdout JSON summary. An operator (or automation) watching only the
exit code + stderr, per Unix convention, has no indication of *why* the
run failed or which run id to inspect.

ACCEPTANCE (bead rev-oswpo):
  - Run `dark-factory ... --goal "<test>"` with an invalid goal; verify
    exit code != 0.
  - stderr contains clear failure summary.
  - Successful dispatch still exits 0.
"""

from __future__ import annotations

import io
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.__main__ import main  # noqa: E402


def _write_looping_pipeline(tmp_path: pathlib.Path) -> pathlib.Path:
    """A pipeline whose `ping` node always reports success but always
    re-enters itself, guaranteeing `max_visits` is exceeded and the run
    ends in the synthetic `exhausted` outcome (see test_loop_bounds.py for
    the same pattern at the engine layer)."""
    dot = tmp_path / "loop.dot"
    dot.write_text(
        'digraph loop {\n'
        '  graph [goal="loop" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  ping [type="codergen", label="Ping", max_visits="2"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> ping\n'
        '  ping -> ping  [condition="outcome=success"]\n'
        '  ping -> exit\n'
        '}\n'
    )
    return dot


def _run_cli(argv: list[str], tmp_path: pathlib.Path, monkeypatch):
    monkeypatch.setattr(pathlib.Path, "home", lambda: tmp_path / "_home")
    stdout_buf = io.StringIO()
    stderr_buf = io.StringIO()
    monkeypatch.setattr(sys, "stdout", stdout_buf)
    monkeypatch.setattr(sys, "stderr", stderr_buf)
    rc = main(argv)
    return rc, stdout_buf.getvalue(), stderr_buf.getvalue()


def test_exhausted_dispatch_exits_nonzero_with_stderr_summary(tmp_path, monkeypatch):
    dot = _write_looping_pipeline(tmp_path)
    rc, stdout_text, stderr_text = _run_cli(
        [
            "--pipeline", str(dot),
            "--goal", "exhausted goal",
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--no-perf-log",
        ],
        tmp_path,
        monkeypatch,
    )

    # Exit code must be non-zero on a failed/exhausted dispatch.
    assert rc != 0

    summary = json.loads(stdout_text)
    assert summary["final_outcome"] == "exhausted"
    run_id = summary["run_id"]

    # stderr must carry a clear, human-readable failure summary — not just
    # a buried field in the stdout JSON blob.
    assert stderr_text.strip(), "expected a non-empty stderr failure summary"
    assert "Dispatch failed" in stderr_text
    assert "exhausted" in stderr_text
    assert run_id in stderr_text


def test_successful_dispatch_exits_zero_with_empty_stderr(tmp_path, monkeypatch):
    dot = tmp_path / "simple.dot"
    dot.write_text(
        'digraph simple {\n'
        '  graph [goal="simple" goal_gate=false]\n'
        '  start [shape=Mdiamond]\n'
        '  one [type="codergen"]\n'
        '  exit [shape=Msquare]\n'
        '  start -> one -> exit\n'
        '}\n'
    )
    rc, stdout_text, stderr_text = _run_cli(
        [
            "--pipeline", str(dot),
            "--goal", "successful goal",
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--no-perf-log",
        ],
        tmp_path,
        monkeypatch,
    )

    assert rc == 0
    summary = json.loads(stdout_text)
    assert summary["final_outcome"] == "success"
    assert stderr_text == ""
