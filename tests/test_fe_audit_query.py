import json
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
FE_AUDIT_QUERY = REPO_ROOT / "daemon" / "scripts" / "fe_audit_query.py"


def test_g11_cancelled_query(tmp_path):
    """Test fe_audit_query.py g11_cancelled extracts CANCELLED bead IDs."""
    log_file = tmp_path / "daemon.jsonl"
    cutoff = "2026-08-20T00:00:00Z"

    records = [
        # In lookback, CANCELLED -> should match
        {"timestamp": "2026-08-21T12:00:00Z", "beadId": "bead-cancelled-1", "lifecycleState": "CANCELLED"},
        {"timestamp": "2026-08-21T13:00:00Z", "beadId": "bead-cancelled-2", "lifecycleState": "CANCELLED"},
        # Duplicate CANCELLED bead -> should be deduplicated
        {"timestamp": "2026-08-21T14:00:00Z", "beadId": "bead-cancelled-1", "lifecycleState": "CANCELLED"},
        # Prior to cutoff -> should NOT match
        {"timestamp": "2026-08-19T12:00:00Z", "beadId": "bead-cancelled-old", "lifecycleState": "CANCELLED"},
        # Different state (HUMAN_HELD, ATTESTED, DISPATCHED) -> should NOT match
        {"timestamp": "2026-08-21T12:00:00Z", "beadId": "bead-held-1", "lifecycleState": "HUMAN_HELD"},
        {"timestamp": "2026-08-21T12:00:00Z", "beadId": "bead-attested-1", "lifecycleState": "ATTESTED"},
        {"timestamp": "2026-08-21T12:00:00Z", "beadId": "bead-dispatched-1", "lifecycleState": "DISPATCHED"},
    ]

    with open(log_file, "w", encoding="utf-8") as fh:
        for rec in records:
            fh.write(json.dumps(rec) + "\n")

    res = subprocess.run(
        [sys.executable, str(FE_AUDIT_QUERY), "g11_cancelled", str(log_file), cutoff],
        capture_output=True,
        text=True,
    )

    assert res.returncode == 0, f"Command failed: {res.stderr}"
    matched_ids = res.stdout.strip().splitlines()
    assert matched_ids == ["bead-cancelled-1", "bead-cancelled-2"]


def test_fe_audit_query_all_queries_and_edge_cases(tmp_path):
    """Test all queries in fe_audit_query.py including malformed records and edge cases."""
    log_file = tmp_path / "daemon.jsonl"
    cutoff = "2026-08-20T00:00:00Z"

    records = [
        {"eventType": "TICK", "timestamp": "2026-08-21T10:00:00Z"},
        {"eventType": "TICK", "timestamp": "2026-08-21T11:00:00Z"},
        {"timestamp": "2026-08-21T10:00:00Z", "beadId": "b1", "lifecycleState": "ATTESTED"},
        {"timestamp": "2026-08-21T10:00:00Z", "beadId": "b1", "lifecycleState": "DISPATCHED"},
        {"timestamp": "2026-08-21T10:00:00Z", "beadId": "b2", "lifecycleState": "HUMAN_HELD"},
        {"timestamp": "2026-08-21T10:00:00Z", "beadId": "b3", "lifecycleState": "CANCELLED"},
        {"eventType": "TASK_DISPATCHED", "timestamp": "2026-08-21T10:05:00Z"},
        {"eventType": "BEAD_TRANSIENT_ERROR", "timestamp": "2026-08-21T10:10:00Z", "beadId": "b4"},
        {"eventType": "BEAD_TRANSIENT_ERROR", "timestamp": "2026-08-21T10:15:00Z", "beadId": "b4"},
    ]

    with open(log_file, "w", encoding="utf-8") as fh:
        fh.write("\n{malformed json\n")
        for rec in records:
            fh.write(json.dumps(rec) + "\n")
        fh.write("\n")

    # g10_ticks
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY), "g10_ticks", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["2026-08-21T10:00:00Z", "2026-08-21T11:00:00Z"]

    # g11_attested
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY), "g11_attested", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["b1"]

    # g11_dispatched
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY), "g11_dispatched", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["b1"]

    # g11_human_held
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY), "g11_human_held", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["b2"]

    # g11_cancelled
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY), "g11_cancelled", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["b3"]

    # g12_transient
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY), "g12_transient", str(log_file), cutoff, "2"], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["2 b4"]

    # g13_dispatch_rate
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY), "g13_dispatch_rate", str(log_file), cutoff, "0"], capture_output=True, text=True, check=True)
    assert "2026-08-21T10: 1 dispatches" in res.stdout


def test_fe_audit_query_error_codes(tmp_path):
    """Test error exit codes for unknown query and invalid args."""
    dummy_log = tmp_path / "dummy.jsonl"
    dummy_log.touch()

    # Invalid args (too few args -> 2)
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY)], capture_output=True, text=True)
    assert res.returncode == 2

    # Unknown query -> 3
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY), "unknown_query", str(dummy_log), "2026-08-20T00:00:00Z"], capture_output=True, text=True)
    assert res.returncode == 3
