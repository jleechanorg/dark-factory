"""Tests for the codergen ``commands_run.md`` receipt source (#406).

Task 1 (commit 84f55a2) added a mechanical producer that parses the
``commands_run.md`` artifact out of a coder worktree into structured
receipt records stashed on ``ctx.state["<node>.structured_receipt"]``.
Task 2 (commit 1a9e286) taught the verdict gate to honor those records
at the SAME trust tier as engine-captured receipts: a passing record
(exit_code==0 + head_sha match) in EITHER source holds a reviewer PASS.

These tests exercise the producer (``_stash_codergen_receipt``) in
isolation by pointing an echo-backend ``Context`` at a temp worktree
controlling the HEAD SHA via ``runner.handlers._worktree_head_sha``, then
exercise the consumer (``_enforce_reproduction_receipt``) by passing the
produced records through ``metadata["_codergen_receipts"]``. The dispatch
site (``_run_gate_once``) is deliberately NOT exercised here — it needs
subprocess mocking; the plan says to drive via ``ctx.state`` instead.
"""

from __future__ import annotations

import pathlib
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.handler_core import Result, Context  # noqa: E402
from runner.parser import Node  # noqa: E402
# Import via handlers first to avoid circular import at test collection time
# (same pattern as tests/test_reviewer_reproduction_receipt.py).
import runner.handlers  # noqa: F401 - forces full module init before handler_codergen
from runner.handler_codergen import (  # noqa: E402
    _stash_codergen_receipt,
    _parse_commands_run_md,
)
from runner.handler_parallel_reviewer import (  # noqa: E402
    _enforce_reproduction_receipt,
)

# A fixed 40-char SHA used as the known HEAD across cases. The producer
# reads it via the late-binding ``runner.handlers._worktree_head_sha`` shim,
# so monkeypatching that symbol controls the value without a real git repo.
KNOWN_SHA = "deadbeefcafebabe1234567890abcdef12345678"
NODE_NAME = "coder_codergen"


def _make_node(name: str = NODE_NAME) -> Node:
    """Minimal codergen-shaped node — only ``.name`` and ``.attrs`` are read."""
    return Node(name=name, attrs={})


def _write_commands_run(workdir: pathlib.Path, body: str) -> pathlib.Path:
    path = workdir / "commands_run.md"
    path.write_text(body, encoding="utf-8")
    return path


class TestProducerStashCodergenReceipt:
    """Producer side: ``_stash_codergen_receipt`` parses commands_run.md
    into structured records on ``ctx.state``."""

    def test_runner_exit0_attaches_structured_record(
        self, tmp_path, monkeypatch
    ):
        """(a) commands_run.md with a runner command + exit code: 0 => one
        structured record attached with the right shape, exit_code==0, and
        head_sha from the worktree."""
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: KNOWN_SHA
        )
        _write_commands_run(
            tmp_path,
            "$ uv run pytest -q\n12 passed\nexit code: 0\n",
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")

        _stash_codergen_receipt(node, ctx)

        key = f"{node.name}.structured_receipt"
        assert key in ctx.state
        records = ctx.state[key]
        assert isinstance(records, list)
        assert len(records) == 1
        rec = records[0]
        # Shape produced by _stash_codergen_receipt.
        assert rec["command"] == ["uv run pytest -q"]
        assert rec["cwd"] == str(tmp_path)
        assert rec["exit_code"] == 0
        assert rec["head_sha"] == KNOWN_SHA
        assert rec["lane_id"] == node.name

    def test_nonzero_exit_recorded_with_exit_code_1(
        self, tmp_path, monkeypatch
    ):
        """(b) only nonzero exits => the record is still attached (the
        producer is mechanical; downgrade happens at the consumer)."""
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: KNOWN_SHA
        )
        _write_commands_run(
            tmp_path,
            "$ uv run pytest -q\n2 failed\nexit code: 1\n",
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")

        _stash_codergen_receipt(node, ctx)

        records = ctx.state[f"{node.name}.structured_receipt"]
        assert len(records) == 1
        assert records[0]["exit_code"] == 1
        assert records[0]["head_sha"] == KNOWN_SHA

    def test_absent_file_leaves_key_unset(self, tmp_path, monkeypatch):
        """(c) no commands_run.md in the worktree => the structured_receipt
        key is never set on ctx.state (existing behavior unchanged)."""
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: KNOWN_SHA
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")

        _stash_codergen_receipt(node, ctx)

        assert f"{node.name}.structured_receipt" not in ctx.state

    def test_malformed_lines_skipped_without_crash(
        self, tmp_path, monkeypatch
    ):
        """(d) a mix of valid + orphan exit + command-with-no-exit + garbage
        => only the ONE valid (command, exit) pair is parsed; no crash."""
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: KNOWN_SHA
        )
        _write_commands_run(
            tmp_path,
            (
                "$ uv run pytest -q\n"
                "12 passed\n"
                "exit code: 0\n"
                "exit code: 5\n"          # orphan: no preceding command
                "$ no exit code here\n"    # command with no following exit
                "this is just garbage\n"   # unparseable line
            ),
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")

        _stash_codergen_receipt(node, ctx)

        records = ctx.state[f"{node.name}.structured_receipt"]
        assert len(records) == 1
        assert records[0]["command"] == ["uv run pytest -q"]
        assert records[0]["exit_code"] == 0

    def test_parser_unit_mixed_forms(self):
        """Direct unit check on the mechanical parser: the three accepted
        exit-line forms (``exit code``, ``exit_code``, ``exit``) all parse,
        and the producer carries them through unchanged."""
        text = (
            "$ a\nexit code: 0\n"
            "$ b\nexit_code: 2\n"
            "$ c\nexit: 7\n"
        )
        pairs = _parse_commands_run_md(text)
        assert pairs == [("a", 0), ("b", 2), ("c", 7)]


class TestConsumerEnforceReproductionReceipt:
    """Consumer side: ``_enforce_reproduction_receipt`` honors codergen
    receipts at the same trust tier as engine receipts."""

    def test_codergen_receipt_exit0_holds_success(
        self, tmp_path, monkeypatch
    ):
        """(a) end-to-end: producer stashes an exit-0 record, the records
        are passed through ``_codergen_receipts`` metadata, and the
        reviewer PASS is held (result returned unchanged)."""
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: KNOWN_SHA
        )
        _write_commands_run(
            tmp_path,
            "$ uv run pytest -q\n12 passed\nexit code: 0\n",
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")
        _stash_codergen_receipt(node, ctx)
        records = ctx.state[f"{node.name}.structured_receipt"]

        result = Result(
            outcome="success",
            output="Reviewed the diff. All good.\nVerdict: PASS",
            metadata={"verdict": "pass", "_codergen_receipts": records},
        )
        adjusted = _enforce_reproduction_receipt(
            result, expected_sha=KNOWN_SHA
        )
        # The codergen receipt at the same trust tier holds the PASS.
        assert adjusted is result
        assert adjusted.outcome == "success"
        assert adjusted.metadata.get("receipt_path") == "structured"

    def test_codergen_receipt_nonzero_downgrades(
        self, tmp_path, monkeypatch
    ):
        """(b) only nonzero exits in the codergen receipt => the reviewer
        PASS is downgraded to failure via the structured path, with the
        nonzero exit surfaced in the gap message."""
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: KNOWN_SHA
        )
        _write_commands_run(
            tmp_path,
            "$ uv run pytest -q\n2 failed\nexit code: 1\n",
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")
        _stash_codergen_receipt(node, ctx)
        records = ctx.state[f"{node.name}.structured_receipt"]

        result = Result(
            outcome="success",
            output="Reviewed the diff. All good.\nVerdict: PASS",
            metadata={"verdict": "pass", "_codergen_receipts": records},
        )
        adjusted = _enforce_reproduction_receipt(
            result, expected_sha=KNOWN_SHA
        )
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["receipt_downgraded"] == "true"
        assert adjusted.metadata["receipt_path"] == "structured"
        # The gap message mentions the nonzero exit code.
        assert "nonzero" in adjusted.metadata["receipt_gap"].lower()
        assert "1" in adjusted.metadata["receipt_gap"]

    def test_no_structured_source_regex_fallback_downgrades(
        self, tmp_path, monkeypatch
    ):
        """(c) absent commands_run.md => no structured source, so the
        regex fallback fires on narrative-only output and downgrades with
        ``receipt_path="regex_low_trust"``."""
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: KNOWN_SHA
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")
        # No commands_run.md written => producer leaves key unset.
        _stash_codergen_receipt(node, ctx)
        assert f"{node.name}.structured_receipt" not in ctx.state

        # No _codergen_receipts and no _reviewer_receipts in metadata =>
        # regex fallback path is taken.
        result = Result(
            outcome="success",
            output="Reviewed the diff. Looks correct.\nVerdict: PASS",
            metadata={"verdict": "pass"},
        )
        adjusted = _enforce_reproduction_receipt(
            result, expected_sha=KNOWN_SHA
        )
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["receipt_downgraded"] == "true"
        assert adjusted.metadata["receipt_path"] == "regex_low_trust"

    def test_sha_mismatch_downgrades_structured(
        self, tmp_path, monkeypatch
    ):
        """Edge case: a codergen receipt with exit 0 but a head_sha that
        does not match expected_sha must downgrade via the structured path
        (a stale receipt cannot be reused across rerolls)."""
        other_sha = "0" * 40
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: other_sha
        )
        _write_commands_run(
            tmp_path,
            "$ uv run pytest -q\n12 passed\nexit code: 0\n",
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")
        _stash_codergen_receipt(node, ctx)
        records = ctx.state[f"{node.name}.structured_receipt"]
        # Sanity: the producer captured the other SHA.
        assert records[0]["head_sha"] == other_sha

        result = Result(
            outcome="success",
            output="Reviewed the diff.\nVerdict: PASS",
            metadata={"verdict": "pass", "_codergen_receipts": records},
        )
        adjusted = _enforce_reproduction_receipt(
            result, expected_sha=KNOWN_SHA
        )
        assert adjusted.outcome == "failure"
        assert adjusted.metadata["receipt_path"] == "structured"
        assert "mismatch" in adjusted.metadata["receipt_gap"].lower()

    def test_engine_receipt_alongside_codergen_aggregates(
        self, tmp_path, monkeypatch
    ):
        """Edge case: a failing engine receipt + a passing codergen receipt
        both at the same trust tier => a pass in EITHER source holds the
        PASS (OR-aggregation across both sources)."""
        monkeypatch.setattr(
            "runner.handlers._worktree_head_sha", lambda p: KNOWN_SHA
        )
        _write_commands_run(
            tmp_path,
            "$ uv run pytest -q\n12 passed\nexit code: 0\n",
        )
        node = _make_node()
        ctx = Context(goal="t", workdir=tmp_path, backend="echo")
        _stash_codergen_receipt(node, ctx)
        codergen_records = ctx.state[f"{node.name}.structured_receipt"]

        # Engine receipt with same SHA but nonzero exit (a failed setup step).
        engine_records = [
            {
                "command": ["bash setup.sh"],
                "cwd": str(tmp_path),
                "exit_code": 1,
                "head_sha": KNOWN_SHA,
                "lane_id": "engine",
            }
        ]
        result = Result(
            outcome="success",
            output="Reviewed the diff.\nVerdict: PASS",
            metadata={
                "verdict": "pass",
                "_reviewer_receipts": engine_records,
                "_codergen_receipts": codergen_records,
            },
        )
        adjusted = _enforce_reproduction_receipt(
            result, expected_sha=KNOWN_SHA
        )
        # The codergen receipt's exit-0 holds the PASS despite the engine
        # receipt's nonzero exit.
        assert adjusted is result
        assert adjusted.outcome == "success"
