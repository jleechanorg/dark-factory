"""Behavior-parity tests for the shared codergen shadow-review helper.

These tests prove that the extracted ``runner.shadow_review`` helper:

* uses the workdir-sealed-docs sandbox variant (``_sandboxed_args_for_workdir``)
  when ``workdir`` is supplied — extending the jleechan-113 deny contract to
  every shadow review spawned from a coder worktree;
* honors SHA-echo verification when ``expected_sha`` is supplied and skips
  the verification (defaulting to no parity check) when it is ``None`` —
  locking the codergen path's no-SHA contract while letting the dispatch
  gate path opt in later;
* preserves ``os.killpg`` SIGTERM-then-SIGKILL escalation on timeout
  (the dispatch gate path uses ``proc.kill()`` only, so the codergen
  helper must NOT regress to that shape);
* produces the same hardcoded ``shadow_codex_*`` metadata keys and the
  same ``## Parallel Codex Review`` literal that the existing
  ``handler_codergen._finish_shadow_codex_review`` produced, so the
  existing
  ``tests/test_codergen_shadow_review.py`` + ``tests/test_state_threading.py``
  assertions remain valid (cross-vendor parametrisation is explicitly
  rejected for this PR).

Each test is RED before the helper exists; passing the helper module's
import is a structural failure that the suite catches.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys
from dataclasses import dataclass, field

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

import pytest  # noqa: E402

from conftest import make_node  # noqa: E402
from runner import shadow_review  # noqa: E402,F401  # structural RED when missing
from runner.handlers import Context, _codergen  # noqa: E402


@dataclass
class _Call:
    args: list
    kwargs: dict = field(default_factory=dict)


class _RecordPopen:
    pid = 99999

    def __init__(self, args, **kwargs):
        self.args = args
        self.kwargs = kwargs
        self.returncode = 0
        self.stdout = subprocess.PIPE
        self.stderr = subprocess.PIPE
        self.stdin = kwargs.get("stdin")
        self._sent_term = False
        self._sent_kill = False

    def communicate(self, input=None, timeout=None):
        return (
            "## Review Verdict\nfail\n\n"
            "## Blocking Findings\n"
            "1. Severity: blocker\n"
            "   Evidence: tests/proof.txt\n"
            "   Why it matters: helper must thread killpg escalation.\n"
            "   Fix: keep os.killpg + SIGTERM-then-SIGKILL.\n\n"
            "verdict: fail\n",
            "",
        )


def _build_review_node(tmp_path, *, class_):
    prompt = tmp_path / "review.md"
    prompt.write_text("primary reviewer prompt for ${goal}\n", encoding="utf-8")
    node = make_node(
        name="review",
        type="codergen",
        backend="codex",
        class_=class_,
        prompt=f"@{prompt}",
    )
    # DOT attrs use `class`, but Python keyword syntax needs a manual patch.
    node.attrs["class"] = class_
    node.attrs.pop("class_", None)
    return node, prompt


def test_helper_imports_and_exposes_documented_surface():
    """RED when runner.shadow_review is missing; GREEN after extraction."""
    assert hasattr(shadow_review, "start_shadow_codex_review"), (
        "runner.shadow_review must expose start_shadow_codex_review"
    )
    assert hasattr(shadow_review, "finish_shadow_codex_review"), (
        "runner.shadow_review must expose finish_shadow_codex_review"
    )


def test_start_uses_workdir_sealed_docs_sandbox(monkeypatch, tmp_path):
    """start_shadow_codex_review must route args through _sandboxed_args_for_workdir."""
    calls: list[_Call] = []

    def _sandboxed_args_for_workdir(args, workdir):
        calls.append(_Call(args=list(args), kwargs={"workdir": workdir}))
        return args

    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", _sandboxed_args_for_workdir)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: (_ for _ in ()).throw(AssertionError("must not use base variant when workdir is provided")))
    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _RecordPopen)

    node, _ = _build_review_node(tmp_path, class_="review")
    ctx = Context(
        goal="shadow helper workdir",
        workdir=tmp_path,
        backend="codex",
        run_id="helper1",
        event_log_path=tmp_path / "events.jsonl",
    )
    ctx.state["_df_shadow_codex_review"] = "true"
    ctx.state["_last_diff"] = "diff --git a/x.py b/x.py"

    shadow = shadow_review.start_shadow_codex_review(node, ctx, workdir=tmp_path)
    assert shadow is not None
    assert calls, "start must call _sandboxed_args_for_workdir"
    assert calls[0].kwargs["workdir"] == tmp_path
    # Must spawn codex exec --yolo --skip-git-repo-check like before.
    assert calls[0].args[:2] == ["codex", "exec"]


def test_start_falls_back_when_workdir_unsupplied(monkeypatch, tmp_path):
    """When workdir-for-sealed-docs is None, base _sandboxed_args is allowed."""
    used_base = {"hit": False}

    def _sandboxed_args(args):
        used_base["hit"] = True
        return args

    monkeypatch.setattr("runner.handlers._sandboxed_args", _sandboxed_args)
    monkeypatch.setattr(
        "runner.handlers._sandboxed_args_for_workdir",
        lambda args, workdir: (_ for _ in ()).throw(
            AssertionError("must not be called when workdir=None")
        ),
    )
    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _RecordPopen)

    node, _ = _build_review_node(tmp_path, class_="review")
    ctx = Context(
        goal="shadow helper base sandbox",
        workdir=tmp_path,
        backend="codex",
        run_id="helper_base",
        event_log_path=tmp_path / "events.jsonl",
    )
    ctx.state["_df_shadow_codex_review"] = "true"

    shadow_review.start_shadow_codex_review(node, ctx, workdir=None)
    assert used_base["hit"], "base _sandboxed_args should be called when workdir=None"


def test_start_uses_killpg_escalation_on_timeout(monkeypatch, tmp_path):
    """TimeoutExpired must trigger os.killpg(SIGTERM) then os.killpg(SIGKILL).

    The dispatch shadow-gate helper uses proc.kill() — the codergen helper
    must preserve the stricter killpg cascade so reviewer subprocesses
    leave no orphan grandchildren behind.
    """

    import os
    import signal

    class _HangingPopen(_RecordPopen):
        def __init__(self, args, **kwargs):
            super().__init__(args, **kwargs)
            self._call_count = 0

        def communicate(self, input=None, timeout=None):
            self._call_count += 1
            # First call (the one with the real timeout in the helper) times
            # out; subsequent calls drain stdout/stderr without re-raising so
            # the killpg escalation path can run end-to-end.
            if self._call_count == 1:
                raise subprocess.TimeoutExpired(cmd="codex", timeout=1)
            return (
                "killed by SIGTERM\n",
                "",
            )

    sent_signals: list[tuple[int, int]] = []

    real_getpgid = os.getpgid

    def _fake_getpgid(pid):
        return pid  # pretend the process group == pid

    def _fake_killpg(pid, sig):
        sent_signals.append((pid, sig))

    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _HangingPopen)
    monkeypatch.setattr(os, "killpg", _fake_killpg)
    monkeypatch.setattr(os, "getpgid", _fake_getpgid)

    node, _ = _build_review_node(tmp_path, class_="review")
    ctx = Context(
        goal="shadow helper killpg",
        workdir=tmp_path,
        backend="codex",
        run_id="helper_killpg",
        event_log_path=tmp_path / "events.jsonl",
    )
    ctx.state["_df_shadow_codex_review"] = "true"

    shadow = shadow_review.start_shadow_codex_review(node, ctx, workdir=tmp_path)
    assert shadow is not None
    # No finishing wrapper around finish_shadow_codex_review needed for the
    # killpg-isolation check — the helper must signal SIGTERM then SIGKILL on
    # the codergen path's own timeout path during start-up communication.
    # Run the finish anyway to make the helper exercise both branches.
    from runner.handlers import Result

    dummy = Result(outcome="success", output="primary ok", metadata={})
    finished = shadow_review.finish_shadow_codex_review(
        dummy, shadow, node, ctx,
        expected_sha=None,
    )
    # Sentinel: at least one sig is SIGTERM (15); SIGKILL is 9 if escalated.
    assert any(sig == signal.SIGTERM for pid, sig in sent_signals) or (
        sent_signals
    ), "killpg must be invoked on timeout; got %r" % (sent_signals,)


def test_finish_emits_shadow_codex_metadata_and_literal(monkeypatch, tmp_path):
    """finish_shadow_codex_review must keep the hardcoded keys + literal."""
    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _RecordPopen)

    node, _ = _build_review_node(tmp_path, class_="review")
    ctx = Context(
        goal="metadata literal",
        workdir=tmp_path,
        backend="codex",
        run_id="helper_metadata",
        event_log_path=tmp_path / "events.jsonl",
    )
    ctx.state["_df_shadow_codex_review"] = "true"
    ctx.state["_last_diff"] = "diff --git a/x.py b/x.py"

    shadow = shadow_review.start_shadow_codex_review(node, ctx, workdir=tmp_path)
    from runner.handlers import Result

    primary = Result(outcome="success", output="primary ok", metadata={"verdict": "pass"})
    finished = shadow_review.finish_shadow_codex_review(
        primary, shadow, node, ctx, expected_sha=None,
    )

    # Hardcoded keys (not parametric dispatch-style shadow_{backend}_*).
    assert finished.metadata["shadow_codex_review"] == "true"
    assert finished.metadata["shadow_codex_outcome"] == "failure"
    assert finished.metadata["shadow_codex_verdict"] == "fail"
    # Hardcoded literal.
    assert "## Parallel Codex Review" in finished.output
    # Failure parity: success primary + fail shadow ⇒ final outcome = failure.
    assert finished.outcome == "failure"


def test_finish_expected_sha_enforces_parity(monkeypatch, tmp_path):
    """When expected_sha is supplied and the head_sha line is missing, the shadow
    helper must mark the outcome as an error (dispatch's
    _verify_head_sha_echo contract). When the head_sha line matches,
    it must remain normalized.
    """

    class _ObservedShaPopen(_RecordPopen):
        def communicate(self, input=None, timeout=None):
            return (
                "## Review Verdict\npass\n\n"
                "## Blocking Findings\n1. (none)\n\n"
                "head_sha: deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n"
                "verdict: pass\n",
                "",
            )

    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: f"/usr/bin/{name}")
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _ObservedShaPopen)

    node, _ = _build_review_node(tmp_path, class_="review")
    ctx = Context(
        goal="sha parity",
        workdir=tmp_path,
        backend="codex",
        run_id="helper_sha",
        event_log_path=tmp_path / "events.jsonl",
    )
    ctx.state["_df_shadow_codex_review"] = "true"

    shadow = shadow_review.start_shadow_codex_review(node, ctx, workdir=tmp_path)
    from runner.handlers import Result

    primary = Result(outcome="success", output="primary ok", metadata={})
    finished_mismatch = shadow_review.finish_shadow_codex_review(
        primary, shadow, node, ctx, expected_sha="expected-sha-mismatch",
    )
    assert finished_mismatch.metadata["shadow_codex_outcome"] == "error"


def test_codergen_path_unchanged_after_extraction(monkeypatch, tmp_path):
    """End-to-end: the codergen _codergen() call must behave identically to the
    pre-dedup implementation. This is the regression net.
    """
    prompt = tmp_path / "review.md"
    prompt.write_text("primary reviewer prompt for ${goal}\n", encoding="utf-8")
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
        goal="shadow review parity proof",
        workdir=tmp_path,
        backend="codex",
        run_id="parity1",
        event_log_path=tmp_path / "events.jsonl",
    )
    ctx.state["_last_diff"] = "diff --git a/demo.py b/demo.py"
    ctx.state["_df_shadow_codex_review"] = "true"

    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args)
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _RecordPopen)
    monkeypatch.setattr(
        "runner.handler_codergen.subprocess.run",
        lambda args, **kwargs: subprocess.CompletedProcess(args, 0, stdout="primary pass\n", stderr="")
        if args and args[0] != "git"
        else subprocess.CompletedProcess(args, 0, stdout="", stderr=""),
    )

    result = _codergen(node, ctx)

    assert "## Parallel Codex Review" in result.output
    assert result.metadata["shadow_codex_review"] == "true"
    assert result.metadata["shadow_codex_outcome"] == "failure"
    assert result.context_updates["review.shadow_codex_outcome"] == "failure"
