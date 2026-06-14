"""Tests for run_id isolation in CXDB and Healer, and exit node enforcement in evidence bundles."""

import json
import pathlib
import sqlite3
from unittest.mock import patch

import pytest

from runner.cxdb import CXDB
from runner.healer import _clusters, report, main
from runner.evidence import write_bundle
from runner.parser import Node, Graph


def test_cxdb_run_id_isolation(tmp_path):
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    
    # Setup Run 1: has failure in implement node
    run1_id = db.start_run(pipeline="pipeline1", goal="test run 1")
    db.record_step(
        run_id=run1_id,
        seq=0,
        node="implement",
        outcome="failure",
        ts=100.0,
        output="Error in run 1",
        metadata={"tokens_in": 10, "wall_ms": 100},
    )
    db.end_run(run1_id, "failure")

    # Setup Run 2: has failure in plan node
    run2_id = db.start_run(pipeline="pipeline2", goal="test run 2")
    db.record_step(
        run_id=run2_id,
        seq=0,
        node="plan",
        outcome="failure",
        ts=200.0,
        output="Error in run 2",
        metadata={"tokens_in": 20, "wall_ms": 200},
    )
    db.end_run(run2_id, "failure")
    db.close()

    # Query without run_id filtering
    db_read = CXDB(db_path)
    try:
        all_failed = list(db_read.failed_steps())
        assert len(all_failed) == 2
        nodes = {f["node"] for f in all_failed}
        assert nodes == {"implement", "plan"}

        # Query filtering by run1_id
        run1_failed = list(db_read.failed_steps(run_id=run1_id))
        assert len(run1_failed) == 1
        assert run1_failed[0]["node"] == "implement"

        # Query filtering by run2_id
        run2_failed = list(db_read.failed_steps(run_id=run2_id))
        assert len(run2_failed) == 1
        assert run2_failed[0]["node"] == "plan"

        # Check cluster aggregates with and without run_id
        # Let's get output hashes
        h1 = [f["output_hash"] for f in all_failed if f["node"] == "implement"][0]
        h2 = [f["output_hash"] for f in all_failed if f["node"] == "plan"][0]

        agg_all = db_read.cluster_aggregates("implement", "failure", h1)
        assert agg_all["total_tokens"] == 10

        agg_run1 = db_read.cluster_aggregates("implement", "failure", h1, run_id=run1_id)
        assert agg_run1["total_tokens"] == 10

        agg_run2 = db_read.cluster_aggregates("implement", "failure", h1, run_id=run2_id)
        assert agg_run2["total_tokens"] is None or agg_run2["total_tokens"] == 0
    finally:
        db_read.close()


def test_healer_run_id_isolation(tmp_path):
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    
    run1_id = db.start_run(pipeline="pipeline1", goal="test run 1")
    db.record_step(
        run_id=run1_id,
        seq=0,
        node="implement",
        outcome="failure",
        ts=100.0,
        output="Error in run 1",
        metadata={"tokens_in": 10, "wall_ms": 100},
    )
    db.end_run(run1_id, "failure")

    run2_id = db.start_run(pipeline="pipeline2", goal="test run 2")
    db.record_step(
        run_id=run2_id,
        seq=0,
        node="plan",
        outcome="failure",
        ts=200.0,
        output="Error in run 2",
        metadata={"tokens_in": 20, "wall_ms": 200},
    )
    db.end_run(run2_id, "failure")
    db.close()

    # Healer report with run_id = run1_id
    rep1 = report(db_path, backend="echo", run_id=run1_id)
    assert "implement" in rep1
    assert "plan" not in rep1

    # Healer report with run_id = run2_id
    rep2 = report(db_path, backend="echo", run_id=run2_id)
    assert "plan" in rep2
    assert "implement" not in rep2

    # CLI test
    with patch("sys.stdout") as mock_stdout:
        main(["--cxdb", str(db_path), "--run-id", run1_id])
        # Verify healer was called and printed output
        args, _ = mock_stdout.write.call_args_list[0]
        output = args[0]
        assert "implement" in output
        assert "plan" not in output


def test_evidence_exit_node_check(tmp_path):
    # Setup two mock graphs: one with exit node, one without
    exit_node = Node(name="exit", attrs={"shape": "Msquare"})
    implement_node = Node(name="implement", attrs={"shape": "ellipse"})
    
    graph_with_exit = Graph(
        name="with_exit",
        goal="test",
        nodes={"implement": implement_node, "exit": exit_node},
        edges=[],
    )
    
    graph_without_exit = Graph(
        name="no_exit",
        goal="test",
        nodes={"implement": implement_node},
        edges=[],
    )

    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    
    # 1. Run that completed but has NO step matching an exit node
    run1_id = db.start_run(pipeline="pipeline", goal="no exit test")
    db.record_step(
        run_id=run1_id,
        seq=0,
        node="implement",
        outcome="success",
        ts=100.0,
        output="Done implement step",
        metadata={},
    )
    db.end_run(run1_id, "success")

    # 2. Run that completed and has a step matching an exit node
    run2_id = db.start_run(pipeline="pipeline", goal="with exit test")
    db.record_step(
        run_id=run2_id,
        seq=0,
        node="implement",
        outcome="success",
        ts=200.0,
        output="Done implement step",
        metadata={},
    )
    db.record_step(
        run_id=run2_id,
        seq=1,
        node="exit",
        outcome="success",
        ts=210.0,
        output="Done exit step",
        metadata={},
    )
    db.end_run(run2_id, "success")
    db.close()

    # Verify run 1 with graph_without_exit
    bundle_dir1 = tmp_path / "bundle1"
    pipeline_dot = tmp_path / "pipeline.dot"
    pipeline_dot.write_text("digraph {}")

    # Call write_bundle on run1 which has no exit node steps
    manifest1 = write_bundle(
        bundle_dir=bundle_dir1,
        cxdb_path=db_path,
        run_id=run1_id,
        pipeline_path=pipeline_dot,
        graph=graph_with_exit,  # graph has exit, but steps do not!
        workdir=tmp_path,
    )
    # final_outcome must be forced to failure
    assert manifest1["final_outcome"] == "failure"

    # Call write_bundle on run2 which has exit node steps
    bundle_dir2 = tmp_path / "bundle2"
    manifest2 = write_bundle(
        bundle_dir=bundle_dir2,
        cxdb_path=db_path,
        run_id=run2_id,
        pipeline_path=pipeline_dot,
        graph=graph_with_exit,  # graph has exit, and steps do too!
        workdir=tmp_path,
    )
    # final_outcome remains success
    assert manifest2["final_outcome"] == "success"
