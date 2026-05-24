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


def test_healer_diagnose_cluster_claude(tmp_path):
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

    # Mock subprocess.run to simulate a successful LLM call
    mock_proc = MagicMock()
    mock_proc.returncode = 0
    mock_proc.stdout = "Evidence-backed prescription: Improve implement.md prompts by adding type annotations."
    mock_proc.stderr = ""

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
