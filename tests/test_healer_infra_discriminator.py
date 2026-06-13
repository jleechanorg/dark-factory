"""Tests for the Healer infra-vs-real failure discriminator.

The discriminator separates "the harness could not run the agent" (infra)
from "the agent ran and produced a bad result" (real). Operators use this
label to decide whether the next action is to fix the harness, the runner,
or the prompt/spec/holdout.
"""

from __future__ import annotations

import pytest

from runner.cxdb import CXDB
from runner.healer import (
    Cluster,
    classify_failure_kind,
    report,
)


# ------------------------------------------------------------------
# Direct unit tests on classify_failure_kind()
# ------------------------------------------------------------------


def _cluster(outcome: str, sample: str = "", metadata: dict | None = None) -> Cluster:
    """Build a minimal Cluster for the classifier. The real Cluster requires
    going through CXDB; for the classifier we only need outcome + sample +
    metadata shape, so a hand-built dataclass instance is fine."""
    return Cluster(
        node="implement",
        outcome=outcome,
        output_hash="deadbeef",
        hits=1,
        sample=sample,
        run_ids=["r1"],
        total_tokens=None,
        total_cost_usd=None,
        total_wall_ms=None,
        metadata=metadata or {},
    )


def test_backend_missing_in_metadata_is_infra():
    """backend_missing=true in metadata is the canonical infra signature."""
    c = _cluster("failure", "anything", {"backend_missing": "true"})
    kind, reason = classify_failure_kind(c)
    assert kind == "infra"
    assert "backend" in reason.lower() or "missing" in reason.lower()


def test_filenotfounderror_in_sample_is_infra():
    """FileNotFoundError in the captured output is an infra failure —
    the agent never got to do work because the harness could not find a file."""
    c = _cluster(
        "failure",
        "Traceback...\nFileNotFoundError: [Errno 2] No such file: /tmp/x\n",
    )
    kind, reason = classify_failure_kind(c)
    assert kind == "infra"
    assert "FileNotFoundError" in reason


def test_command_not_found_in_sample_is_infra():
    """Shell "command not found" means a backend binary is not installed."""
    c = _cluster("failure", "sh: claude: command not found\n")
    kind, reason = classify_failure_kind(c)
    assert kind == "infra"
    assert "command not found" in reason.lower()


def test_modulenotfounderror_in_sample_is_infra():
    """ModuleNotFoundError in the sample means a Python dep is missing —
    this is harness infra, not an agent quality problem."""
    c = _cluster("error", "ModuleNotFoundError: No module named 'anthropic'\n")
    kind, reason = classify_failure_kind(c)
    assert kind == "infra"
    assert "ModuleNotFoundError" in reason


def test_exhausted_outcome_is_real():
    """Exhausted means the agent loop hit max_visits — real work happened,
    the agent just couldn't converge. That's a real (spec/prompt) failure."""
    c = _cluster("exhausted", "still no fix after 3 iterations", {})
    kind, reason = classify_failure_kind(c)
    assert kind == "real"
    assert "exhausted" in reason.lower() or "max_visits" in reason.lower()


def test_failure_with_normal_output_is_real():
    """A failure outcome with substantive output means the agent ran and
    produced a wrong result. That's a real failure."""
    c = _cluster(
        "failure",
        "Compilation failed:\n  File 'src/x.py' line 42\n    name 'y' is not defined",
    )
    kind, reason = classify_failure_kind(c)
    assert kind == "real"
    assert "real" in reason.lower() or "output" in reason.lower()


def test_partial_outcome_is_real():
    """partial means the agent produced a partial result — a real outcome."""
    c = _cluster("partial", "completed step 1 of 3", {})
    kind, _reason = classify_failure_kind(c)
    assert kind == "real"


def test_inconclusive_outcome_is_real():
    """inconclusive means we could not tell whether the agent succeeded;
    by convention this is treated as a real failure (spec needs more
    deterministic scoring)."""
    c = _cluster("inconclusive", "reviewer could not parse verdict", {})
    kind, _reason = classify_failure_kind(c)
    assert kind == "real"


def test_stuck_with_empty_output_is_infra():
    """stuck + empty output_head with a short timeout signature is infra —
    a hang, not a real failure. The agent never produced output."""
    c = _cluster("stuck", "", {"timeout_ms": "1000"})
    kind, reason = classify_failure_kind(c)
    assert kind == "infra"
    assert "stuck" in reason.lower() or "timeout" in reason.lower() or "empty" in reason.lower()


def test_error_with_empty_output_is_infra():
    """error outcome with empty output_head is treated as infra (a crash
    before the agent could run)."""
    c = _cluster("error", "")
    kind, _reason = classify_failure_kind(c)
    assert kind == "infra"


def test_fail_outcome_with_normal_output_is_real():
    """'fail' is treated the same as 'failure' — real work happened."""
    c = _cluster("fail", "holdout test 3/10 passed\n")
    kind, _reason = classify_failure_kind(c)
    assert kind == "real"


def test_stuck_with_substantive_output_is_real():
    """A stuck cluster that still produced output is a real failure
    (the agent looped or produced a non-converging artifact)."""
    c = _cluster("stuck", "iteration 3 still failing assertion X")
    kind, _reason = classify_failure_kind(c)
    assert kind == "real"


def test_failure_with_timed_out_true_is_infra():
    """Timeout failures are marked with outcome='failure' and timed_out='true'
    in metadata. These should be classified as infra (harness timeout), not
    real (agent failure), even when they have non-empty output."""
    c = _cluster(
        "failure",
        "ao spawn timed out after 300 seconds",
        {"timed_out": "true", "timeout": "300"},
    )
    kind, reason = classify_failure_kind(c)
    assert kind == "infra"
    assert "timeout" in reason.lower() or "timed_out" in reason.lower()


# ------------------------------------------------------------------
# Integration: the Healer report must include the kind + justification
# ------------------------------------------------------------------


def _seed_cluster(db: CXDB, *, node: str, outcome: str, output: str, metadata: dict) -> str:
    run_id = db.start_run(pipeline="p", goal="g")
    db.record_step(
        run_id=run_id,
        seq=0,
        node=node,
        outcome=outcome,
        ts=1.0,
        output=output,
        metadata=metadata,
    )
    db.end_run(run_id, outcome)
    return run_id


def test_healer_report_includes_kind_column_for_infra_cluster(tmp_path):
    """The Healer Markdown report must label infra clusters distinctly
    and include a one-line justification, not just the existing columns."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        _seed_cluster(
            db,
            node="codergen",
            outcome="failure",
            output="sh: claude: command not found",
            metadata={},
        )
    finally:
        db.close()

    text = report(db_path, backend="echo")
    assert "infra" in text.lower()
    # And we expect the justification to mention command not found.
    assert "command not found" in text.lower()


def test_healer_report_includes_kind_column_for_real_cluster(tmp_path):
    """Real clusters (exhausted, real failure output) must be labeled
    'real' in the report with a justification referencing the outcome."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        _seed_cluster(
            db,
            node="implement",
            outcome="exhausted",
            output="still no fix after 3 iterations",
            metadata={"max_visits": "3"},
        )
    finally:
        db.close()

    text = report(db_path, backend="echo")
    assert "real" in text.lower()
    # The report's sample section still appears, plus the new kind column.
    assert "exhausted" in text.lower()


def test_healer_report_mixed_clusters_have_both_labels(tmp_path):
    """A mixed-failure CXDB yields both 'infra' and 'real' labels in
    the same report — the discriminator is not all-or-nothing."""
    db_path = tmp_path / "cxdb.sqlite"
    db = CXDB(db_path)
    try:
        _seed_cluster(
            db,
            node="codergen",
            outcome="failure",
            output="FileNotFoundError: /usr/local/bin/agy",
            metadata={"backend_missing": "true"},
        )
        _seed_cluster(
            db,
            node="implement",
            outcome="failure",
            output="compilation failed at line 42",
            metadata={},
        )
    finally:
        db.close()

    text = report(db_path, backend="echo")
    # The infra cluster's justification names the missing-file error.
    assert "FileNotFoundError" in text
    # Both kinds must appear in the report.
    assert "infra" in text.lower()
    assert "real" in text.lower()
