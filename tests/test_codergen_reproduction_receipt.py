"""Tests for codergen-side reproduction receipt instrumentation (issue #406 / #441).

Issue #406 originally asked for a file-backed commands_run.md contract integrated
through runner/handler_codergen.py::_codergen so that codergen-backed review
nodes would also produce the engine-captured receipt the structured gate
verifies. PR #407/#425/#426 implemented the structured receipt path on the
review-gate subprocess (runner/handler_dispatch._run_gate_once) but left
codergen untouched: every codergen subprocess completed without calling
``_record_reviewer_receipt``, so a ``codergen`` node used as a review node
could not be receipt-checked even when ``receipt_required="true"`` was set.

This test module closes that gap end-to-end:

  1. _codergen records a structured receipt for every subprocess backend
     (codex, claude, agy) when the subprocess completes.
  2. The recorded receipt binds command + cwd + exit_code + head_sha +
     lane_id + output_sha256 — the five fields the #406 contract calls out.
  3. Fabricated subprocess results are not laundered: a proc whose
     returncode != 0 cannot be promoted into an exit_code=0 receipt.
  4. Cross-lane (primary + shadow) receipts are kept distinct by lane_id and
     are both visible to the structured check.
  5. The optional commands_run.md sidecar file is written when requested
     so graphs that need a durable artifact have one.
  6. Stale-head receipts (head_sha != worktree HEAD) are recorded but fail
     the structured gate.

The integration path uses the real ``_codergen`` function (not a stub),
monkey-patches the backend-specific ``subprocess.run`` / ``Popen`` paths,
and verifies ``ctx.state["_reviewer_receipts"]`` plus the Result.metadata
of the underlying gate subprocess path.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from conftest import make_node  # noqa: E402
from runner.handlers import Context, _codergen  # noqa: E402


_REAL_SUBPROCESS_RUN_REF = subprocess.run


def _make_fake_agy_binary(tmp_path: pathlib.Path) -> pathlib.Path:
    """Write a tiny shell script that pretends to be ``agy`` and exits 0.

    The agy codergen path invokes ``agy --print ...``. Putting a fake on PATH
    avoids monkeypatching ``subprocess.Popen``, which would also intercept
    the receipt helper's ``git rev-parse`` call.
    """
    bin_dir = tmp_path / "fake_bin"
    bin_dir.mkdir(exist_ok=True)
    agy = bin_dir / "agy"
    agy.write_text("#!/bin/sh\necho 'agy stdout transcript'\nexit 0\n")
    agy.chmod(0o755)
    return bin_dir


def _git_init(path: pathlib.Path) -> str:
    """Initialize a real git repo at ``path`` and return its HEAD SHA."""
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    subprocess.run(
        ["git", "-C", str(path), "config", "user.email", "test@example.com"],
        check=True,
    )
    subprocess.run(
        ["git", "-C", str(path), "config", "user.name", "test"], check=True,
    )
    (path / "README.md").write_text("hello")
    subprocess.run(["git", "-C", str(path), "add", "-A"], check=True)
    subprocess.run(
        ["git", "-C", str(path), "commit", "-q", "-m", "init"], check=True,
    )
    out = subprocess.run(
        ["git", "-C", str(path), "rev-parse", "HEAD"],
        check=True, capture_output=True, text=True,
    )
    return out.stdout.strip()


def _build_ctx(tmp_path: pathlib.Path, *, head_sha: str | None = None) -> Context:
    if head_sha is None:
        head_sha = _git_init(tmp_path)
    event_log = tmp_path / "events.jsonl"
    ctx = Context(
        goal="receipt proof",
        workdir=tmp_path,
        backend="codex",
        run_id="receiptproof",
        event_log_path=event_log,
    )
    ctx.state["_last_diff"] = "diff --git a/x b/x"
    return ctx


def _patch_codergen_subprocess(monkeypatch, *, fake_proc: subprocess.CompletedProcess | _FakePopen, patch_popen: bool = False, pass_env: bool = False):
    """Patch the sandbox + subprocess shims so _codergen runs cleanly in tests.

    The receipt helper resolves ``git rev-parse HEAD`` via a fresh import of
    ``subprocess``. ``subprocess.run`` internally uses ``subprocess.Popen``,
    so when tests patch Popen the helper's ``subprocess.run`` call inherits
    the stub — which doesn't speak git. We leave Popen untouched by default
    and rely on a discriminating ``_fake_run`` that handles git separately.
    Only callers that exercise the agy Popen path opt in to ``patch_popen``.
    ``pass_env`` (used by agy tests) keeps the real ``os.environ`` so a
    fake binary on PATH is reachable.
    """
    monkeypatch.setattr("runner.handler_codergen.shutil.which", lambda name: "/usr/bin/codex")
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: args)
    monkeypatch.setattr(
        "runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args,
    )
    if pass_env:
        import os as _os
        monkeypatch.setattr("runner.handlers._sanitized_env", lambda: dict(_os.environ))
    else:
        monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    if patch_popen:
        monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _FakePopen)

    # Differentiate git (used by _stash_diff AND the receipt's head_sha lookup)
    # from codergen invocation. git invocations go to the REAL git binary
    # via ``os.popen`` so the SHA binding survives end-to-end; everything
    # else is the codergen subprocess and returns fake_proc.
    def _fake_run(args, **kwargs):
        if args and args[0] == "git":
            import os as _os
            try:
                cmd_str = " ".join(args) + " 2>/dev/null"
                stdout = _os.popen(cmd_str).read().strip()
                rc = 0 if stdout else 1
            except Exception:
                stdout = ""
                rc = 1
            return subprocess.CompletedProcess(args, rc, stdout=stdout, stderr="")
        return fake_proc

    monkeypatch.setattr("runner.handler_codergen.subprocess.run", _fake_run)


# ---------------------------------------------------------------------------
# (1) Receipt instrumentation
# ---------------------------------------------------------------------------


class TestCodergenRecordsReceipt:
    """The codergen handler must emit a structured receipt per subprocess."""

    def test_codex_backend_records_receipt(self, tmp_path, monkeypatch):
        head_sha = _git_init(tmp_path)
        prompt = tmp_path / "review.md"
        prompt.write_text("primary reviewer prompt for ${goal}\n", encoding="utf-8")
        node = make_node(
            name="review_node",
            type="codergen",
            backend="codex",
            prompt=f"@{prompt}",
        )
        ctx = _build_ctx(tmp_path, head_sha=head_sha)
        proc = subprocess.CompletedProcess(
            ["codex", "exec", "--yolo", "--skip-git-repo-check", prompt.read_text()],
            0, stdout="primary reviewer says pass\n", stderr="",
        )
        _patch_codergen_subprocess(monkeypatch, fake_proc=proc)

        result = _codergen(node, ctx)

        assert result.outcome == "success"
        receipts = ctx.state.get("_reviewer_receipts")
        assert isinstance(receipts, list) and len(receipts) == 1, (
            "codergen must record a structured receipt after subprocess.run completes"
        )
        rec = receipts[0]
        assert rec["command"][0] == "codex"
        assert rec["cwd"] == str(tmp_path)
        assert rec["exit_code"] == 0
        assert rec["head_sha"] == head_sha
        assert rec["lane_id"] == "review_node"
        assert "output_sha256" in rec
        # Output sha256 binds the receipt to the actual subprocess output text.
        expected_sha = hashlib.sha256(b"primary reviewer says pass\n").hexdigest()
        assert rec["output_sha256"] == expected_sha

    def test_claude_backend_records_receipt(self, tmp_path, monkeypatch):
        head_sha = _git_init(tmp_path)
        prompt = tmp_path / "review.md"
        prompt.write_text("claude reviewer prompt\n", encoding="utf-8")
        node = make_node(
            name="claude_review",
            type="codergen",
            backend="claude",
            prompt=f"@{prompt}",
        )
        ctx = _build_ctx(tmp_path, head_sha=head_sha)
        proc = subprocess.CompletedProcess(
            ["claude", "--print", "..."], 0, stdout="claude pass\n", stderr="",
        )
        _patch_codergen_subprocess(monkeypatch, fake_proc=proc)
        # Make the claude executable lookup return a predictable name.
        monkeypatch.setattr(
            "runner.handlers._get_claude_executable", lambda: "claude",
        )

        result = _codergen(node, ctx)

        assert result.outcome == "success"
        receipts = ctx.state.get("_reviewer_receipts")
        assert len(receipts) == 1
        assert receipts[0]["command"][0] == "claude"
        assert receipts[0]["lane_id"] == "claude_review"
        assert receipts[0]["exit_code"] == 0

    def test_agy_backend_records_receipt(self, tmp_path, monkeypatch):
        head_sha = _git_init(tmp_path)
        prompt = tmp_path / "review.md"
        prompt.write_text("agy reviewer prompt\n", encoding="utf-8")
        node = make_node(
            name="agy_review",
            type="codergen",
            backend="agy",
            prompt=f"@{prompt}",
        )
        ctx = _build_ctx(tmp_path, head_sha=head_sha)
        # Provide a fake `agy` binary on PATH so the codergen subprocess
        # actually runs (instead of failing with FileNotFoundError). This
        # avoids monkeypatching ``subprocess.Popen``, which would also
        # intercept the receipt helper's ``git rev-parse`` call.
        bin_dir = _make_fake_agy_binary(tmp_path)
        monkeypatch.setenv("PATH", f"{bin_dir}:{__import__('os').environ.get('PATH', '')}")
        _patch_codergen_subprocess(monkeypatch, fake_proc=None, pass_env=True)

        result = _codergen(node, ctx)

        if result.outcome != "success":
            print(f"AGY FAIL: outcome={result.outcome} output={result.output[:300]!r}")
        assert result.outcome == "success"
        receipts = ctx.state.get("_reviewer_receipts")
        assert len(receipts) == 1
        assert receipts[0]["command"][0] == "agy"
        assert receipts[0]["exit_code"] == 0
        assert receipts[0]["head_sha"] == head_sha


# ---------------------------------------------------------------------------
# (2) Fabricated and failed receipts
# ---------------------------------------------------------------------------


class TestCodergenReceiptIntegrity:
    """The receipt must reflect the actual subprocess outcome, not a fabricated claim."""

    def test_nonzero_exit_cannot_be_promoted(self, tmp_path, monkeypatch):
        """A subprocess that exited 1 must produce an exit_code=1 receipt — no laundering."""
        head_sha = _git_init(tmp_path)
        prompt = tmp_path / "review.md"
        prompt.write_text("primary reviewer prompt\n", encoding="utf-8")
        node = make_node(
            name="review",
            type="codergen",
            backend="codex",
            prompt=f"@{prompt}",
        )
        ctx = _build_ctx(tmp_path, head_sha=head_sha)
        proc = subprocess.CompletedProcess(
            ["codex", "exec", "--yolo", "--skip-git-repo-check", "..."],
            1, stdout="codex failed\n", stderr="some error",
        )
        _patch_codergen_subprocess(monkeypatch, fake_proc=proc)

        result = _codergen(node, ctx)

        # The receipt must reflect the real exit code (not a fabricated zero).
        receipts = ctx.state.get("_reviewer_receipts")
        assert len(receipts) == 1
        assert receipts[0]["exit_code"] == 1
        # And the gate's verdict must reflect that, too — a nonzero receipt
        # cannot satisfy the structured receipt check.
        assert result.outcome in ("failure", "error")


# ---------------------------------------------------------------------------
# (3) Stale-head receipts
# ---------------------------------------------------------------------------


class TestStaleHeadReceipt:
    """A receipt recorded under a stale HEAD SHA must NOT satisfy the gate."""

    def test_stale_sha_receipt_fails_structured_check(self, tmp_path):
        from runner.handler_verdict import (
            _check_structured_receipt,
            _record_reviewer_receipt,
        )

        class _Ctx:
            def __init__(self) -> None:
                self.state: dict = {}
                self.workdir = tmp_path

        ctx = _Ctx()
        stale_sha = "0000000000000000000000000000000000000000"
        current_sha = "abcdef0123456789abcdef0123456789abcdef01"
        _record_reviewer_receipt(
            ctx,
            command=["pytest"],
            cwd=str(tmp_path),
            exit_code=0,
            head_sha=stale_sha,
            lane_id="primary",
        )
        gap = _check_structured_receipt(ctx, expected_sha=current_sha)
        assert "head_sha mismatch" in gap


# ---------------------------------------------------------------------------
# (4) Cross-lane receipts
# ---------------------------------------------------------------------------


class TestCrossLaneReceipts:
    """Primary and shadow reviewer lanes must each record their own receipt."""

    def test_two_lanes_have_distinct_receipts(self, tmp_path):
        from runner.handler_verdict import (
            _check_structured_receipt,
            _record_reviewer_receipt,
        )

        class _Ctx:
            def __init__(self) -> None:
                self.state: dict = {}
                self.workdir = tmp_path

        ctx = _Ctx()
        sha = "abcdef0123456789abcdef0123456789abcdef01"
        for lane in ("primary", "shadow_codex"):
            _record_reviewer_receipt(
                ctx, command=["pytest"], cwd=str(tmp_path),
                exit_code=0, head_sha=sha, lane_id=lane,
            )
        assert _check_structured_receipt(ctx, expected_sha=sha) == ""
        receipts = ctx.state["_reviewer_receipts"]
        assert {r["lane_id"] for r in receipts} == {"primary", "shadow_codex"}


# ---------------------------------------------------------------------------
# (5) Optional commands_run.md sidecar
# ---------------------------------------------------------------------------


class TestCommandsRunSidecar:
    """A durable commands_run.md file is written when the gate requests it."""

    def test_sidecar_written_when_requested(self, tmp_path):
        from runner.handler_verdict import (
            _record_reviewer_receipt,
            write_commands_run_sidecar,
        )

        run_id = "sidecarproof"
        # Create the run dir the sidecar writer expects.
        run_dir = pathlib.Path.home() / ".dark-factory" / "runs" / run_id
        run_dir.mkdir(parents=True, exist_ok=True)

        record = {
            "command": ["uv", "run", "pytest"],
            "cwd": str(tmp_path),
            "exit_code": 0,
            "head_sha": "abcdef0123456789abcdef0123456789abcdef01",
            "lane_id": "primary",
            "output_sha256": "deadbeef" * 8,
            "ts": "2026-07-21T00:00:00Z",
        }
        path_str = write_commands_run_sidecar(
            run_id=run_id,
            node_name="review",
            attempt=1,
            receipts=[record],
        )
        try:
            assert path_str is not None
            path = pathlib.Path(path_str)
            assert path.exists()
            content = path.read_text()
            assert "# commands_run.md" in content
            assert "primary" in content
            assert "abcdef01" in content
            assert "uv run pytest" in content
        finally:
            if path_str:
                p = pathlib.Path(path_str)
                if p.exists():
                    p.unlink()

    def test_sidecar_writes_one_line_per_receipt(self, tmp_path):
        from runner.handler_verdict import write_commands_run_sidecar

        run_id = "sidecarproof2"
        run_dir = pathlib.Path.home() / ".dark-factory" / "runs" / run_id
        run_dir.mkdir(parents=True, exist_ok=True)

        records = [
            {
                "command": ["pytest"],
                "cwd": str(tmp_path),
                "exit_code": 0,
                "head_sha": "abcdef0123456789abcdef0123456789abcdef01",
                "lane_id": "primary",
            },
            {
                "command": ["pytest"],
                "cwd": str(tmp_path),
                "exit_code": 0,
                "head_sha": "abcdef0123456789abcdef0123456789abcdef01",
                "lane_id": "shadow_codex",
            },
        ]
        path_str = write_commands_run_sidecar(
            run_id=run_id, node_name="review", attempt=1, receipts=records,
        )
        try:
            assert path_str is not None
            text = pathlib.Path(path_str).read_text()
            assert text.count("- lane_id:") == 2
            assert "shadow_codex" in text
        finally:
            if path_str:
                p = pathlib.Path(path_str)
                if p.exists():
                    p.unlink()