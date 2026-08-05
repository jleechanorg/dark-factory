"""Behavior-parity tests for the codergen/dispatch shadow-review dedup (bead jleechan-txdh).

This file pins the observable *behavioral* contract across the two
shadow-review pipelines so a future refactor can't silently collapse
the surfaces or drop a sealed-doc deny rule. It does NOT pin
implementation details — only the before/after behavior that must hold
across the dedup. The bead-acceptance tests
(``tests/test_codergen_shadow_review.py`` + ``tests/test_state_threading.py``)
remain the source of truth for the *literal* ``shadow_codex_*`` key
names and ``## Parallel Codex Review`` output marker.

Three contracts are pinned:

  1. ``handler_codergen`` shadow path:
       * Literal metadata keys (``shadow_codex_review``,
         ``shadow_codex_outcome``, etc.).
       * ``## Parallel Codex Review`` marker in output.
       * ``os.killpg(SIGTERM)`` then ``os.killpg(SIGKILL)`` escalation on timeout.
       * NO ``*_head_sha_status`` key (codergen does not enforce SHA-echo).

  2. ``handler_dispatch`` shadow path:
       * Parameterized metadata keys (``shadow_<backend>_gate_*``).
       * ``## Parallel {label} Gate Review`` marker in output.
       * ``*_head_sha_status`` key set to ``matched``/``mismatched``/``missing``.
       * MUST escalate ``os.killpg(SIGTERM)`` then ``os.killpg(SIGKILL)`` on
         timeout — codergen does, dispatch must (was ``proc.kill()`` only).
       * Sandbox-args builder must accept an optional ``workdir`` so the
         sealed-doc deny rules cover benchmark docs in ``ctx.workdir``
         (jleechan-113 contract).

  3. SHA-echo / ``expected_sha`` parity (the asymmetric part):
       * codergen leaves ``expected_sha`` empty (no SHA-echo).
       * dispatch enforces expected_sha via ``_verify_head_sha_echo`` and
         records ``*_head_sha_status``.
"""

from __future__ import annotations

import json
import os
import pathlib
import signal
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import make_node  # noqa: E402
from runner.handlers import Context, _codergen  # noqa: E402


# ---------------------------------------------------------------------------
# Shared fakes
# ---------------------------------------------------------------------------


class _FakePopen:
    """Records args/communicate calls so tests can assert escalation behavior."""

    pid = 12345
    call_log: list = []

    def __init__(self, *args, **kwargs):
        type(self).call_log.append({"args": args[0], "kwargs_keys": sorted(kwargs.keys())})
        self.args = args[0]
        self.returncode = 0

    def communicate(self, timeout=None):
        return (
            "## Review Verdict\nfail\n\nverdict: fail\n",
            "",
        )


def _reset_call_log():
    _FakePopen.call_log = []


class _TimeoutPopen:
    """A Popen that always raises TimeoutExpired on communicate (forces escalation path)."""

    pid = 54321

    def __init__(self, *args, **kwargs):
        self.args = args[0]
        self.returncode = None

    def communicate(self, timeout=None):
        if timeout is not None and "stdin" in [k for k in self.args]:
            pass
        raise subprocess.TimeoutExpired(self.args, timeout)

    def kill(self):
        # dispatch originally used only `proc.kill()`. We assert it's NOT called.
        raise AssertionError(
            "dispatch shadow path must escalate via os.killpg(SIGTERM) → SIGKILL, not proc.kill()"
        )


# ---------------------------------------------------------------------------
# (1) handler_codergen shadow path
# ---------------------------------------------------------------------------


def test_codergen_uses_legacy_shadow_keys_no_head_sha_status(tmp_path, monkeypatch):
    """Pins: codergen keeps literal ``shadow_codex_*`` keys and never sets ``*_head_sha_status``."""
    _reset_call_log()
    prompt = tmp_path / "review.md"
    prompt.write_text("primary reviewer prompt for ${goal}\n", encoding="utf-8")
    event_log = tmp_path / "events.jsonl"
    node = make_node(
        name="review",
        type="codergen",
        backend="codex",
        class_="review",
        prompt=f"@{prompt}",
    )
    node.attrs["class"] = "review"
    node.attrs.pop("class_", None)
    ctx = Context(
        goal="codergen parity",
        workdir=tmp_path,
        backend="codex",
        run_id="codergenparity",
        event_log_path=event_log,
    )
    ctx.state["_last_diff"] = "diff --git a/x.py b/x.py"
    ctx.state["_last_changed_files"] = "- x.py"
    ctx.state["_df_shadow_codex_review"] = "true"

    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: args)
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _FakePopen)

    def _fake_run(args, **kwargs):
        if args and args[0] == "git":
            return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
        return subprocess.CompletedProcess(args, 0, stdout="primary reviewer says pass\n", stderr="")

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", _fake_run)

    result = _codergen(node, ctx)

    # Literal metadata keys preserved.
    assert result.metadata["shadow_codex_review"] == "true"
    assert result.metadata["shadow_codex_outcome"] == "failure"
    assert result.metadata["shadow_codex_verdict"] == "fail"
    # NO ``head_sha_status`` — codergen has no expected_sha contract.
    assert "shadow_codex_head_sha_status" not in result.metadata
    # Output literal preserved.
    assert "## Parallel Codex Review" in result.output


def test_codergen_escalates_killpg_sigterm_then_sigkill_on_timeout(tmp_path, monkeypatch):
    """Pins: codergen shadow path escalates via os.killpg SIGTERM → SIGKILL on timeout."""
    import runner.handler_codergen as codergen_mod

    kills: list = []

    def _fake_killpg(pid, sig):
        kills.append((pid, sig))
        # ProcessLookupError on SIGTERM means the inner drain call never
        # happens and we escalate to SIGKILL. Track the count so the test
        # can verify exactly two killpg attempts.
        raise ProcessLookupError

    seq_log: list = []

    class _TimeoutPopenLog:
        pid = 99999

        def __init__(self, *args, **kwargs):
            self.args = args[0]
            self.returncode = None
            self._call_count = 0

        def communicate(self, timeout=None):
            self._call_count += 1
            # First call: simulate timeout so codergen enters the except branch.
            # Second+ calls (drain after killpg): return empty so the escalate
            # completes without raising.
            if self._call_count == 1:
                raise subprocess.TimeoutExpired(self.args, timeout)
            return ("", "")

    prompt = tmp_path / "review.md"
    prompt.write_text("primary reviewer prompt for ${goal}\n", encoding="utf-8")
    event_log = tmp_path / "events.jsonl"
    node = make_node(
        name="review",
        type="codergen",
        backend="codex",
        class_="review",
        prompt=f"@{prompt}",
    )
    node.attrs["class"] = "review"
    node.attrs.pop("class_", None)
    ctx = Context(
        goal="killpg codergen",
        workdir=tmp_path,
        backend="codex",
        run_id="killpgcodergen",
        event_log_path=event_log,
    )
    ctx.state["_last_diff"] = "diff"
    ctx.state["_last_changed_files"] = "- x.py"
    ctx.state["_df_shadow_codex_review"] = "true"
    # Use a very short timeout to drive the escalate branch fast.
    ctx.state["_df_shadow_codex_timeout"] = "1"

    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: args)
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr(codergen_mod.os, "killpg", _fake_killpg)
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _TimeoutPopenLog)

    def _fake_run(args, **kwargs):
        if args and args[0] == "git":
            return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
        return subprocess.CompletedProcess(args, 0, stdout="primary pass\n", stderr="")

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", _fake_run)

    _codergen(node, ctx)

    # First escalate must be SIGTERM, fallback must be SIGKILL.
    assert kills, "expected killpg to be called at least once"
    assert kills[0] == (99999, signal.SIGTERM), f"first escalate should be SIGTERM, got {kills[0]}"
    assert any(sig == signal.SIGKILL for _, sig in kills), "expected SIGKILL escalate"


# ---------------------------------------------------------------------------
# (2) handler_dispatch shadow path
# ---------------------------------------------------------------------------


def test_dispatch_sandbox_args_builder_accepts_optional_workdir(monkeypatch):
    """Pins: handler_dispatch's sandbox-arg builder accepts an optional ``workdir``
    for the sealed-docs deny rule (jleechan-113 contract).

    Without this extension the dispatch shadow path can never access
    sealed docs in ``ctx.workdir`` even though the codergen path does.
    """
    from runner.handler_dispatch import _gate_subprocess_args
    from runner import handlers as handlers_mod

    captured: dict = {}

    def _fake_sandboxed_args_for_workdir(args, workdir):
        captured["args"] = list(args)
        captured["workdir"] = workdir
        # Return a marker so we can verify the dispatch build calls into it.
        return ["sandbox-marker"] + list(args)

    def _fake_sandboxed_args(args):
        captured.setdefault("legacy_args", list(args))
        return list(args)

    monkeypatch.setattr(handlers_mod, "_sandboxed_args", _fake_sandboxed_args)
    monkeypatch.setattr(handlers_mod, "_sandboxed_args_for_workdir", _fake_sandboxed_args_for_workdir)
    monkeypatch.setattr(handlers_mod, "_get_claude_executable", lambda: "claude")

    class _StubCtx:
        workdir = pathlib.Path("/tmp/workdir-jleechan-txdh")

    result = _gate_subprocess_args("codex", "prompt", _StubCtx(), 60, workdir=pathlib.Path("/tmp/workdir-jleechan-txdh"))
    assert result is not None
    assert "sandbox-marker" in result
    assert captured.get("workdir") == pathlib.Path("/tmp/workdir-jleechan-txdh")
    # The dispatch path codex argv is preserved through the marker.
    assert "codex" in result
    assert "exec" in result


def test_dispatch_shadow_path_killpg_sigterm_then_sigkill(tmp_path, monkeypatch):
    """Pins: dispatch shadow path escalates via os.killpg(SIGTERM) → SIGKILL on timeout.

    The pre-dedup dispatch path did ``proc.kill()`` only. The dedup
    adopts codergen's escalation. This test pins the new behavior.
    """
    import runner.handler_dispatch as dispatch_mod

    prompt_path = tmp_path / "shadow_prompt.txt"
    prompt_path.write_text("shadow prompt")
    output_path = tmp_path / "shadow_output.txt"
    output_path.write_text("")

    kills: list = []

    def _fake_killpg(pid, sig):
        kills.append((pid, sig))
        # Both killpg calls raise so the dispatch path goes through
        # SIGTERM → catch → SIGKILL → catch → unconditional drain.
        raise ProcessLookupError

    class _TimeoutPopenDispatch:
        pid = 77777

        def __init__(self, *args, **kwargs):
            self.args = args[0]
            self.returncode = None
            self._call_count = 0

        def communicate(self, input=None, timeout=None):
            self._call_count += 1
            # Drive the SIGTERM-then-SIGKILL escalation:
            #   call 1: timeout=remaining → TimeoutExpired (killpg SIGTERM)
            #   call 2: timeout=5 drain → TimeoutExpired (killpg SIGKILL)
            #   call 3: unconditional drain → empty
            if self._call_count in (1, 2):
                raise subprocess.TimeoutExpired(self.args, timeout)
            return ("", "")

    from runner.handler_dispatch import _start_shadow_gate_review, _finish_shadow_gate_review
    from runner.handler_core import Result as _Result

    event_log = tmp_path / "events.jsonl"
    ctx = Context(
        goal="dispatch killpg",
        workdir=tmp_path,
        backend="codex",
        run_id="dispatchkillpg",
        event_log_path=event_log,
    )
    ctx.state["_df_shadow_codex_review"] = "true"

    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: args)
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr(dispatch_mod.os, "killpg", _fake_killpg)

    # Stub observability sidecar writes so we keep this self-contained.
    from runner import engine_observability as _obs

    monkeypatch.setattr(
        _obs, "_write_input_sidecar",
        lambda ctx, seq, name, attempt, body, kind: (str(tmp_path / f"{kind}-{seq}.txt"), "deadbeef"),
    )
    monkeypatch.setattr(_obs, "_emit_event", lambda *args, **kwargs: None)
    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _TimeoutPopenDispatch)

    shadow = _start_shadow_gate_review(
        name="review",
        prompt="primary",
        expected_sha="abc123",
        timeout=1,
        ctx=ctx,
    )
    assert shadow is not None
    assert shadow.proc is not None, "Popen must have run; launch_error=%r" % shadow.launch_error

    final = _finish_shadow_gate_review(
        result=_Result(outcome="success", output="primary pass"),
        shadow=shadow,
        name="review",
        expected_sha="abc123",
        timeout=1,
        ctx=ctx,
    )

    assert kills, "dispatch shadow path must use killpg for escalate"
    assert kills[0] == (77777, signal.SIGTERM), f"first escalate should be SIGTERM, got {kills[0]}"
    assert any(sig == signal.SIGKILL for _, sig in kills), "expected SIGKILL escalate"
    # The outcome should reflect the kill escalation.
    assert final.outcome == "failure", "escalate path should produce failure outcome"
    assert final.metadata["shadow_codex_gate_timed_out"] == "true"


# ---------------------------------------------------------------------------
# (3) SHA-echo / expected_sha parity
# ---------------------------------------------------------------------------


def test_codergen_dispatch_shadow_path_asymmetry_is_documented(tmp_path, monkeypatch):
    """Pins the explicit asymmetric contract:
       * codergen shadow: NO expected_sha / NO head_sha_status.
       * dispatch shadow: requires expected_sha / sets head_sha_status.

    The dedup must NOT collapse these into one — a future refactor that
    adds expected_sha to codergen (or removes it from dispatch) must
    update this test first.
    """
    # ---------- codergen path ----------
    _reset_call_log()
    prompt = tmp_path / "review_cod.md"
    prompt.write_text("primary reviewer prompt for ${goal}\n", encoding="utf-8")
    event_log = tmp_path / "events.jsonl"
    node = make_node(
        name="review_cod",
        type="codergen",
        backend="codex",
        class_="review",
        prompt=f"@{prompt}",
    )
    node.attrs["class"] = "review"
    node.attrs.pop("class_", None)
    ctx_cod = Context(
        goal="asym cod",
        workdir=tmp_path,
        backend="codex",
        run_id="asymcod",
        event_log_path=event_log,
    )
    ctx_cod.state["_last_diff"] = "diff"
    ctx_cod.state["_last_changed_files"] = "- x.py"
    ctx_cod.state["_df_shadow_codex_review"] = "true"

    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: args)
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _FakePopen)

    def _fake_run_cod(args, **kwargs):
        if args and args[0] == "git":
            return subprocess.CompletedProcess(args, 0, stdout="", stderr="")
        return subprocess.CompletedProcess(args, 0, stdout="primary pass\n", stderr="")

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", _fake_run_cod)
    result_cod = _codergen(node, ctx_cod)

    # Codergen asserts NO head_sha_status.
    assert "shadow_codex_head_sha_status" not in result_cod.metadata
    # Codergen asserts the legacy literal output marker.
    assert "## Parallel Codex Review" in result_cod.output

    # ---------- dispatch path ----------
    _reset_call_log()
    from runner.handler_dispatch import _launch_shadow_gate_review, _finish_shadow_gate_review
    from runner.handler_core import Result as _Result
    from runner import engine_observability as _obs
    import runner.handler_dispatch as dispatch_mod

    monkeypatch.setattr(_obs, "_write_input_sidecar", lambda ctx, seq, name, attempt, body, kind: (str(tmp_path / f"{kind}-{seq}.txt"), "deadbeef"))
    monkeypatch.setattr(_obs, "_emit_event", lambda *args, **kwargs: None)
    monkeypatch.setattr("runner.handler_dispatch.shutil.which", lambda name: "/usr/bin/codex")

    class _FakePopenDispatch:
        pid = 33333

        def __init__(self, *args, **kwargs):
            self.args = args[0]
            self.returncode = 0

        def communicate(self, input=None, timeout=None):
            # Emit the expected head_sha echo so dispatch records ``matched``.
            # Use a real 40-char hex SHA so the SHA-echo regex matches.
            _sha = "a" * 40
            return (f"## Review Verdict\npass\n\nhead_sha: {_sha}\n\nverdict: pass\n", "")

    monkeypatch.setattr(dispatch_mod.subprocess, "Popen", _FakePopenDispatch)

    ctx_disp = Context(
        goal="asym disp",
        workdir=tmp_path,
        backend="codex",
        run_id="asymdisp",
        event_log_path=tmp_path / "events-disp.jsonl",
    )
    ctx_disp.state["_df_shadow_codex_review"] = "true"

    shadow = _launch_shadow_gate_review(
        name="gate_es",
        prompt="primary",
        expected_sha="a" * 40,
        timeout=1200,
        ctx=ctx_disp,
        backend="codex",
    )
    assert shadow is not None
    final = _finish_shadow_gate_review(
        result=_Result(outcome="success", output="primary pass"),
        shadow=shadow,
        name="gate_es",
        expected_sha="a" * 40,
        timeout=1200,
        ctx=ctx_disp,
    )

    # Dispatch asserts the parameterized key is present and equals matched.
    assert final.metadata["shadow_codex_gate_head_sha_status"] == "matched"
    # Dispatch asserts the parameterized output literal.
    assert "## Parallel Codex Gate Review" in final.output
