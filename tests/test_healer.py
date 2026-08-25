"""Tests for the LLM-driven Healer diagnosis loop."""

import json
import pathlib
import subprocess
from unittest.mock import MagicMock, patch

import pytest

from runner.cxdb import CXDB
from runner.healer import Cluster, _clusters, diagnose_cluster, report


def test_healer_diagnose_cluster_echo(tmp_path):
    """Verify diagnose_cluster with 'echo' backend returns a deterministic mock prescription."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        run_id = db.start_run(pipeline="test_pipeline", goal="verify healer works")
        db.record_step(
            run_id=run_id,
            seq=0,
            node="implement",
            outcome="failure",
            ts=123.45,
            output="Error: code compilation failed",
            metadata={"returncode": "1"},
        )
        db.end_run(run_id, "failure")
    finally:
        db.close()

    clusters = _clusters(db_path)
    assert len(clusters) == 1
    c = clusters[0]

    rx = diagnose_cluster(c, backend="echo", cxdb_path=db_path)
    assert "Node 'implement' failed with 'failure'" in rx
    assert "pipeline 'test_pipeline'" in rx
    assert "goal: 'verify healer works'" in rx
    assert '"returncode": "1"' in rx


def test_healer_diagnose_cluster_claude(tmp_path, monkeypatch):
    """Verify diagnose_cluster with 'claude' backend calls subprocess.run correctly with the prompt."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        run_id = db.start_run(pipeline="test_pipeline", goal="verify healer works")
        db.record_step(
            run_id=run_id,
            seq=0,
            node="implement",
            outcome="failure",
            ts=123.45,
            output="Error: code compilation failed",
            metadata={"returncode": "1"},
        )
        db.end_run(run_id, "failure")
    finally:
        db.close()

    clusters = _clusters(db_path)
    assert len(clusters) == 1
    c = clusters[0]

    claude_config = tmp_path / "project-claude-config"
    claude_config.mkdir()
    monkeypatch.setenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", str(claude_config))
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", "/home/operator/.claude")
    monkeypatch.setenv("ANTHROPIC_BASE_URL", "https://personal.example.invalid")
    monkeypatch.setenv("ANTHROPIC_API_KEY", "personal-key")
    monkeypatch.setenv("ANTHROPIC_AUTH_TOKEN", "personal-token")
    monkeypatch.setenv("ANTHROPIC_MODEL", "personal-model")
    monkeypatch.setenv("ANTHROPIC_SMALL_FAST_MODEL", "personal-fast")
    monkeypatch.setenv("MINIMAX_API_KEY", "minimax-key")
    monkeypatch.setenv("CLAUDEM_MODE", "1")

    # Mock subprocess.run to simulate a successful LLM call
    mock_proc = MagicMock()
    mock_proc.returncode = 0
    mock_proc.stdout = "Evidence-backed prescription: Improve implement.md prompts by adding type annotations."
    mock_proc.stderr = ""
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: list(args))

    with patch("subprocess.run", return_value=mock_proc) as mock_run:
        rx = diagnose_cluster(c, backend="claude", cxdb_path=db_path)
        assert rx == "Evidence-backed prescription: Improve implement.md prompts by adding type annotations."

        # Verify subprocess.run was called
        mock_run.assert_called_once()
        args, kwargs = mock_run.call_args
        # The first argument should contain 'claude' and '--print'
        cmd_args = args[0]
        assert any("claude" in a for a in cmd_args)
        assert "--print" in cmd_args
        # The prompt containing details must be in the arguments
        prompt = cmd_args[-1]
        assert "Failed Node Details:" in prompt
        assert "implement" in prompt
        assert "test_pipeline" in prompt
        assert "verify healer works" in prompt
        env = kwargs["env"]
        assert env["CLAUDE_CONFIG_DIR"] == str(claude_config.resolve())
        for key in (
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_MODEL",
            "ANTHROPIC_SMALL_FAST_MODEL",
            "MINIMAX_API_KEY",
            "CLAUDEM_MODE",
        ):
            assert key not in env


def test_healer_claude_fails_closed_without_project_config(tmp_path, monkeypatch):
    """An unscoped Healer Claude request must not launch a subprocess."""
    monkeypatch.delenv("DARK_FACTORY_CLAUDE_CONFIG_DIR", raising=False)
    cluster = Cluster(
        node="implement",
        outcome="failure",
        output_hash="hash",
        hits=1,
        sample="failure output",
        run_ids=[],
    )

    with patch("subprocess.run") as mock_run:
        result = diagnose_cluster(cluster, backend="claude")

    assert "DARK_FACTORY_CLAUDE_CONFIG_DIR" in result
    mock_run.assert_not_called()


def test_healer_report_integrates_prescription(tmp_path):
    """Verify healer report incorporates the diagnose_cluster prescription."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        run_id = db.start_run(pipeline="test_pipeline", goal="verify healer works")
        db.record_step(
            run_id=run_id,
            seq=0,
            node="implement",
            outcome="failure",
            ts=123.45,
            output="Error: code compilation failed",
            metadata={"returncode": "1"},
        )
        db.end_run(run_id, "failure")
    finally:
        db.close()

    text = report(db_path, backend="echo")
    assert "implement" in text
    assert "failure" in text
    assert "[mock echo prescription] Node 'implement' failed with 'failure'" in text


# ---------------------------------------------------------------------------
# P-C — exhausted_with_uncommitted_work sub-cluster
# ---------------------------------------------------------------------------


def test_exhausted_with_uncommitted_work_clusters_separately(tmp_path):
    """Two exhaustion events with different uncommitted_files counts MUST land
    in different clusters and emit different prescriptions. P-C's whole point
    is that the operator gets a recovery-flavored prescription when work is
    sitting uncommitted, and a generic one when the tree is clean."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        # Two runs, same node + raw outcome, but different metadata. The
        # Healer clusters by (node, outcome, output_hash), so identical
        # output hash + identical raw outcome would normally collide. We
        # vary the output so each row has its own cluster.
        run_id_clean = db.start_run(pipeline="p1", goal="clean exhaustion")
        db.record_step(
            run_id=run_id_clean,
            seq=0,
            node="fix",
            outcome="exhausted",
            ts=0.0,
            output="max_visits reached, no progress",
            metadata={"max_visits": "3", "uncommitted_files": "0"},
        )
        db.end_run(run_id_clean, "exhausted")

        run_id_dirty = db.start_run(pipeline="p2", goal="dirty exhaustion")
        db.record_step(
            run_id=run_id_dirty,
            seq=0,
            node="fix",
            outcome="exhausted",
            ts=1.0,
            output="max_visits reached, 5 uncommitted files",
            metadata={
                "max_visits": "3",
                "uncommitted_files": "5",
                "uncommitted_insertions": "120",
                "uncommitted_deletions": "8",
                "uncommitted_staged_files": "3",
            },
        )
        db.end_run(run_id_dirty, "exhausted")
    finally:
        db.close()

    clusters = _clusters(db_path)
    assert len(clusters) == 2, "expected exactly two clusters, one per outcome"

    by_node = {c.node: c for c in clusters}
    assert "fix" in by_node
    clean_cluster = next(c for c in clusters if c.metadata.get("uncommitted_files") == "0")
    dirty_cluster = next(c for c in clusters if c.metadata.get("uncommitted_files") == "5")

    # P-C: refined_outcome must diverge on the uncommitted_files metadata.
    assert clean_cluster.refined_outcome == "exhausted"
    assert dirty_cluster.refined_outcome == "exhausted_with_uncommitted_work"

    # The dirty cluster's prescription MUST mention the recovery commands.
    rx_dirty = dirty_cluster.prescription(backend="echo", cxdb_path=db_path)
    assert "git status --porcelain" in rx_dirty
    assert "git fsck --lost-found" in rx_dirty
    assert "uncommitted" in rx_dirty.lower()

    # The clean cluster's prescription stays on the generic echo template.
    rx_clean = clean_cluster.prescription(backend="echo", cxdb_path=db_path)
    assert "git fsck --lost-found" not in rx_clean
    assert "[mock echo prescription]" in rx_clean


def test_exhausted_clean_cluster_keeps_raw_outcome(tmp_path):
    """When uncommitted_files is "0" or empty, refined_outcome must equal the
    raw outcome — backwards compatibility for the report table + classification
    heuristics that already key on the canonical outcome."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        run_id = db.start_run(pipeline="p1", goal="clean exhaustion")
        db.record_step(
            run_id=run_id,
            seq=0,
            node="fix",
            outcome="exhausted",
            ts=0.0,
            output="clean exhaustion",
            metadata={"uncommitted_files": "0"},
        )
        db.end_run(run_id, "exhausted")
    finally:
        db.close()

    clusters = _clusters(db_path)
    assert len(clusters) == 1
    c = clusters[0]
    assert c.outcome == "exhausted"
    assert c.refined_outcome == "exhausted"
    assert c.display_outcome == "exhausted"


def test_healer_report_shows_refined_outcome_for_uncommitted(tmp_path):
    """The report table must show `exhausted_with_uncommitted_work` as the
    Outcome column for the dirty cluster (operator-facing surface)."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        run_id = db.start_run(pipeline="p1", goal="dirty exhaustion")
        db.record_step(
            run_id=run_id,
            seq=0,
            node="fix",
            outcome="exhausted",
            ts=0.0,
            output="dirty exhaustion",
            metadata={
                "uncommitted_files": "3",
                "uncommitted_insertions": "10",
                "uncommitted_deletions": "2",
                "uncommitted_staged_files": "1",
            },
        )
        db.end_run(run_id, "exhausted")
    finally:
        db.close()

    text = report(db_path, backend="echo")
    assert "exhausted_with_uncommitted_work" in text
    # The recovery commands surface in the prescription column.
    assert "git fsck --lost-found" in text
