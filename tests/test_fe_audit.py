import json
import os
import subprocess
import sys
from pathlib import Path
import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent
FE_AUDIT_SCRIPT = REPO_ROOT / "daemon" / "scripts" / "fe-audit.sh"
FE_AUDIT_QUERY_PY = REPO_ROOT / "daemon" / "scripts" / "fe_audit_query.py"

sys.path.insert(0, str(REPO_ROOT / "daemon" / "scripts"))
import fe_audit_query  # noqa: E402


def test_fe_audit_query_g11_cancelled():
    cutoff = "2026-08-01T00:00:00Z"
    records = [
        # Before cutoff
        {
            "timestamp": "2026-07-31T23:59:59Z",
            "beadId": "bead-old-cancelled",
            "lifecycleState": "CANCELLED",
            "eventType": "SKIPPED_DUPLICATE_BEAD",
        },
        # After cutoff, CANCELLED
        {
            "timestamp": "2026-08-01T12:00:00Z",
            "beadId": "bead-cancelled-1",
            "lifecycleState": "CANCELLED",
            "eventType": "SKIPPED_DUPLICATE_BEAD",
        },
        # After cutoff, duplicate record for same bead
        {
            "timestamp": "2026-08-01T12:05:00Z",
            "beadId": "bead-cancelled-1",
            "lifecycleState": "CANCELLED",
            "eventType": "SKIPPED_DUPLICATE_BEAD",
        },
        # After cutoff, another bead CANCELLED
        {
            "timestamp": "2026-08-01T13:00:00Z",
            "beadId": "bead-cancelled-2",
            "lifecycleState": "CANCELLED",
            "eventType": "SKIPPED_DUPLICATE_BEAD",
        },
        # Record with empty beadId
        {
            "timestamp": "2026-08-01T13:30:00Z",
            "beadId": "",
            "lifecycleState": "CANCELLED",
            "eventType": "SKIPPED_DUPLICATE_BEAD",
        },
        # Other states
        {
            "timestamp": "2026-08-01T14:00:00Z",
            "beadId": "bead-attested",
            "lifecycleState": "ATTESTED",
            "eventType": "PR_ATTESTED",
        },
        {
            "timestamp": "2026-08-01T15:00:00Z",
            "beadId": "bead-human-held",
            "lifecycleState": "HUMAN_HELD",
            "eventType": "HUMAN_HELD",
        },
    ]

    cancelled_bids = list(fe_audit_query.g11_cancelled(records, cutoff))
    assert cancelled_bids == ["bead-cancelled-1", "bead-cancelled-1", "bead-cancelled-2"]


def test_fe_audit_query_all_queries(tmp_path):
    log_file = tmp_path / "daemon.jsonl"
    cutoff = "2026-08-01T00:00:00Z"
    records = [
        {"timestamp": "2026-08-01T01:00:00Z", "eventType": "TICK"},
        {"timestamp": "2026-08-01T02:00:00Z", "eventType": "TICK"},
        {"timestamp": "2026-08-01T03:00:00Z", "eventType": "TICK"},
        {"timestamp": "2026-08-01T04:00:00Z", "beadId": "b-att", "lifecycleState": "ATTESTED"},
        {"timestamp": "2026-08-01T05:00:00Z", "beadId": "b-disp", "lifecycleState": "DISPATCHED"},
        {"timestamp": "2026-08-01T06:00:00Z", "beadId": "b-held", "lifecycleState": "HUMAN_HELD"},
        {"timestamp": "2026-08-01T07:00:00Z", "beadId": "b-canc", "lifecycleState": "CANCELLED"},
        {"timestamp": "2026-08-01T08:00:00Z", "beadId": "b-err", "eventType": "BEAD_TRANSIENT_ERROR"},
        {"timestamp": "2026-08-01T08:10:00Z", "beadId": "b-err", "eventType": "BEAD_SPAWN_TRANSIENT_ERROR"},
        {"timestamp": "2026-08-01T09:00:00Z", "eventType": "TASK_DISPATCHED"},
        {"timestamp": "2026-08-01T09:30:00Z", "eventType": "TASK_DISPATCHED"},
    ]
    with open(log_file, "w") as fh:
        for r in records:
            fh.write(json.dumps(r) + "\n")

    # g10_ticks
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g10_ticks", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["2026-08-01T01:00:00Z", "2026-08-01T02:00:00Z", "2026-08-01T03:00:00Z"]

    # g11_attested
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g11_attested", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["b-att"]

    # g11_dispatched
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g11_dispatched", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["b-disp"]

    # g11_human_held
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g11_human_held", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["b-held"]

    # g11_cancelled
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g11_cancelled", str(log_file), cutoff], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["b-canc"]

    # g12_transient
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g12_transient", str(log_file), cutoff, "2"], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["2 b-err"]

    # g13_dispatch_rate
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g13_dispatch_rate", str(log_file), cutoff, "1"], capture_output=True, text=True, check=True)
    assert res.stdout.strip().splitlines() == ["2026-08-01T09: 2 dispatches"]


def test_fe_audit_query_error_handling(tmp_path):
    log_file = tmp_path / "daemon.jsonl"
    log_file.write_text("{\"invalid json\n")

    # Invalid args
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g10_ticks"], capture_output=True, text=True)
    assert res.returncode == 2

    # Unknown query
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "unknown_query", str(log_file), "2026-01-01T00:00:00Z"], capture_output=True, text=True)
    assert res.returncode == 3

    # Non-existent file
    res = subprocess.run([sys.executable, str(FE_AUDIT_QUERY_PY), "g10_ticks", str(tmp_path / "does_not_exist.jsonl"), "2026-01-01T00:00:00Z"], capture_output=True, text=True)
    assert res.returncode == 9


def test_fe_audit_sh_excludes_cancelled_beads(tmp_path):
    log_file = tmp_path / "daemon.jsonl"
    state_dir = tmp_path / "state"
    state_dir.mkdir()

    # Create telemetry with:
    # - bead-1: ATTESTED and DISPATCHED (normal flow -> not stuck)
    # - bead-2: ATTESTED, not dispatched, HUMAN_HELD (held -> excluded from stuck)
    # - bead-3: ATTESTED, not dispatched, CANCELLED (cancelled -> excluded from stuck)
    # - bead-4: ATTESTED, not dispatched, not held, not cancelled (genuinely stuck)
    records = [
        # TICK to keep G10 happy
        {"timestamp": "2026-08-22T18:00:00Z", "eventType": "TICK"},
        # bead-1: ATTESTED then DISPATCHED
        {"timestamp": "2026-08-22T18:01:00Z", "beadId": "bead-1", "lifecycleState": "ATTESTED", "eventType": "INTAKE"},
        {"timestamp": "2026-08-22T18:02:00Z", "beadId": "bead-1", "lifecycleState": "DISPATCHED", "eventType": "DISPATCH"},
        # bead-2: ATTESTED then HUMAN_HELD
        {"timestamp": "2026-08-22T18:03:00Z", "beadId": "bead-2", "lifecycleState": "ATTESTED", "eventType": "INTAKE"},
        {"timestamp": "2026-08-22T18:04:00Z", "beadId": "bead-2", "lifecycleState": "HUMAN_HELD", "eventType": "HELD"},
        # bead-3: ATTESTED then CANCELLED (e.g. branch collision dedup)
        {"timestamp": "2026-08-22T18:05:00Z", "beadId": "bead-3", "lifecycleState": "ATTESTED", "eventType": "INTAKE"},
        {"timestamp": "2026-08-22T18:06:00Z", "beadId": "bead-3", "lifecycleState": "CANCELLED", "eventType": "SKIPPED_DUPLICATE_BEAD"},
        # bead-4: ATTESTED only (no dispatch, no hold, no cancel -> STUCK)
        {"timestamp": "2026-08-22T18:07:00Z", "beadId": "bead-4", "lifecycleState": "ATTESTED", "eventType": "INTAKE"},
    ]

    with open(log_file, "w") as fh:
        for r in records:
            fh.write(json.dumps(r) + "\n")

    env = dict(os.environ)
    env["FE_AUDIT_LOG"] = str(log_file)
    env["FE_AUDIT_STATE_DIR"] = str(state_dir)
    env["LOOKBACK_HOURS"] = "24"
    env["MAX_TICK_GAP_SEC"] = "86400"

    res = subprocess.run(
        ["/bin/bash", str(FE_AUDIT_SCRIPT), "--no-bead"],
        env=env,
        capture_output=True,
        text=True,
    )
    assert res.returncode == 0, f"script failed: stderr={res.stderr}"

    # Verify log output shows exactly 1 stuck bead (bead-4), NOT 2 (bead-3 and bead-4)
    assert "G11: attested=1 (no DISPATCHED follow-up over 24h)" in res.stdout


def test_fe_audit_sh_zero_stuck_when_all_attested_are_cancelled(tmp_path):
    log_file = tmp_path / "daemon.jsonl"
    state_dir = tmp_path / "state"
    state_dir.mkdir()

    # All ATTESTED beads were CANCELLED
    records = [
        {"timestamp": "2026-08-22T18:00:00Z", "eventType": "TICK"},
        {"timestamp": "2026-08-22T18:01:00Z", "beadId": "bead-c1", "lifecycleState": "ATTESTED", "eventType": "INTAKE"},
        {"timestamp": "2026-08-22T18:02:00Z", "beadId": "bead-c1", "lifecycleState": "CANCELLED", "eventType": "SKIPPED_DUPLICATE_BEAD"},
        {"timestamp": "2026-08-22T18:03:00Z", "beadId": "bead-c2", "lifecycleState": "ATTESTED", "eventType": "INTAKE"},
        {"timestamp": "2026-08-22T18:04:00Z", "beadId": "bead-c2", "lifecycleState": "CANCELLED", "eventType": "SKIPPED_DUPLICATE_BEAD"},
    ]

    with open(log_file, "w") as fh:
        for r in records:
            fh.write(json.dumps(r) + "\n")

    env = dict(os.environ)
    env["FE_AUDIT_LOG"] = str(log_file)
    env["FE_AUDIT_STATE_DIR"] = str(state_dir)
    env["LOOKBACK_HOURS"] = "24"
    env["MAX_TICK_GAP_SEC"] = "86400"

    res = subprocess.run(
        ["/bin/bash", str(FE_AUDIT_SCRIPT), "--no-bead"],
        env=env,
        capture_output=True,
        text=True,
    )
    assert res.returncode == 0, f"script failed: stderr={res.stderr}"
    assert "G11: attested=0 (no DISPATCHED follow-up over 24h)" in res.stdout
