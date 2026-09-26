"""Regression test for issue #827 Defect 2.

The `codex` backend branch of `_codergen` (used by any `type="codergen"`
node, e.g. the `fix` node in `pipelines/factory/gates.dot`) must never leave
the child process's stdin attached to a Python-managed pipe that depends on
`subprocess.run(input=...)` closing it. Production evidence (dark-factory
issue #827, OpenAI Codex v0.147.0) showed the codex CLI printing
"Reading additional input from stdin..." and then blocking until the node's
own `timeout` fired (`timed_out: true`), even though `input=""` was passed.

The sibling `claude` branch in this same file already avoids this by
attaching `stdin=subprocess.DEVNULL` directly (an unambiguous, pipe-free
sentinel) instead of routing an empty string through a PIPE. This test
locks the `codex` branch onto the same, already-proven-safe discipline.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402
from runner.handlers import Context, _codergen  # noqa: E402


def test_fix_node_codex_backend_closes_stdin_via_devnull(tmp_path, monkeypatch):
    """The `fix` node (type=codergen, backend=codex) must launch codex with
    `stdin=subprocess.DEVNULL`, never with an `input=` pipe."""
    node = make_node(
        "fix",
        type="codergen",
        prompt=None,
        **{"class": "fix"},
    )
    ctx = Context(goal="iterate on the diff", workdir=tmp_path, backend="codex")

    monkeypatch.setattr(
        "runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args
    )
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})

    captured_kwargs: dict = {}

    def fake_run(args, **kwargs):
        captured_kwargs.update(kwargs)
        return subprocess.CompletedProcess(args, 0, stdout="codex done\n", stderr="")

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", fake_run)

    result = _codergen(node, ctx)

    assert result.outcome == "success"
    # The bug: codex was launched with `input=""` (a PIPE Python must close),
    # which the real codex v0.147.0 binary did not treat as EOF and hung on
    # until node timeout. `stdin=subprocess.DEVNULL` is the unambiguous fix —
    # there is no pipe for the child to wait on at all.
    assert captured_kwargs.get("stdin") is subprocess.DEVNULL, (
        f"expected stdin=subprocess.DEVNULL, got kwargs={captured_kwargs!r}"
    )
    assert "input" not in captured_kwargs or not captured_kwargs["input"], (
        "codex backend must not deliver an empty-string PIPE input — "
        "use stdin=subprocess.DEVNULL instead"
    )
