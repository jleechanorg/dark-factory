"""Tests for G11 startup-intake-without-forced-dispatch (bead jleechan-vhsw).

The G11 audit detects ATTESTED beads that have no DISPATCH follow-up over the
lookback window. The remediation hint says: "Intake sweep must enqueue a
DISPATCH_REQUEST event whenever STATE=ATTESTED rows accumulate beyond the
previous tick's snapshot. Without that, restart cycles leave beads stuck in
ATTESTED with no worker spawn."

This test pins the contract:

1. `fe_audit_query.py` exposes a `g11_dispatch_request` query that returns
   the bead IDs that need a DISPATCH_REQUEST event: ATTESTED beads that are
   NEW since the prior snapshot AND have NOT been DISPATCHED in either
   current or prior windows.
2. `fe-audit.sh` persists `last_sweep_attested` and `last_sweep_dispatched`
   in the state file so the next sweep can compute the delta.
3. `fe-audit.sh` emits a `DISPATCH_REQUEST` event to the telemetry log for
   each new ATTESTED bead (so the always-on auto-factory daemon picks it up
   on its next tick).

Without (1), restart cycles leave beads stuck in ATTESTED with no worker
spawn — the original G11 incident.

TDD: tests are written first (red), then the implementation is added.
"""
from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).parent.parent
QUERY_PY = ROOT / "daemon" / "scripts" / "fe_audit_query.py"
AUDIT_SH = ROOT / "daemon" / "scripts" / "fe-audit.sh"


def _run_query(query: str, log_file: pathlib.Path, cutoff: str, threshold: int = 0) -> list[str]:
    """Run fe_audit_query.py and return the parsed lines (bead IDs)."""
    proc = subprocess.run(
        ["python3", str(QUERY_PY), query, str(log_file), cutoff, str(threshold)],
        capture_output=True,
        text=True,
        check=True,
    )
    return [line.strip() for line in proc.stdout.splitlines() if line.strip()]


def _make_log(events: list[dict], path: pathlib.Path) -> None:
    """Write a JSONL log of events to path."""
    with path.open("w") as fh:
        for ev in events:
            fh.write(json.dumps(ev) + "\n")


def test_g11_dispatch_request_query_returns_new_attested_without_dispatch(tmp_path):
    """g11_dispatch_request must return bead IDs that are:
    - in current ATTESTED but NOT in the prior snapshot (i.e. NEW), AND
    - NOT in current DISPATCHED (i.e. not already on their way).

    This is the gap that the G11 audit detected: restart cycles promote
    QUEUED -> ATTESTED but the dispatch tick never fires, so the bead
    accumulates in ATTESTED forever. The fix is to enqueue a
    DISPATCH_REQUEST event for each newly-ATTESTED bead.
    """
    log = tmp_path / "daemon.jsonl"
    _make_log(
        [
            {"beadId": "jleechan-vxi3", "lifecycleState": "ATTESTED", "timestamp": "2026-08-07T00:00:00Z"},
            {"beadId": "jleechan-vxi3", "lifecycleState": "ATTESTED", "timestamp": "2026-08-07T00:00:01Z"},
            {"beadId": "jleechan-old1", "lifecycleState": "ATTESTED", "timestamp": "2026-08-07T00:00:02Z"},
            {"beadId": "jleechan-new1", "lifecycleState": "ATTESTED", "timestamp": "2026-08-07T00:00:03Z"},
            {"beadId": "jleechan-new2", "lifecycleState": "ATTESTED", "timestamp": "2026-08-07T00:00:04Z"},
            # old1 was already dispatched in the prior window; it should NOT
            # be flagged. It still appears ATTESTED now because the
            # verifier keeps attesting while the PR is open.
            {"beadId": "jleechan-old1", "lifecycleState": "DISPATCHED", "timestamp": "2026-08-07T00:00:05Z"},
        ],
        log,
    )

    # Cutoff covers the entire log.
    bead_ids = _run_query("g11_dispatch_request", log, "2026-08-06T00:00:00Z")

    # vxi3 (the G11 bead) and new1/new2 must be flagged. old1 must NOT
    # because it has a DISPATCHED event.
    assert "jleechan-vxi3" in bead_ids, f"vxi3 missing; got {bead_ids}"
    assert "jleechan-new1" in bead_ids, f"new1 missing; got {bead_ids}"
    assert "jleechan-new2" in bead_ids, f"new2 missing; got {bead_ids}"
    assert "jleechan-old1" not in bead_ids, f"old1 should not be flagged; got {bead_ids}"


def test_g11_dispatch_request_query_empty_when_log_empty(tmp_path):
    """Empty log -> no dispatch requests.
    Regression guard: the audit must not synthesize fake beads from an
    empty telemetry log on a fresh install.
    """
    log = tmp_path / "daemon.jsonl"
    log.write_text("")
    bead_ids = _run_query("g11_dispatch_request", log, "2026-08-06T00:00:00Z")
    assert bead_ids == []


def test_g11_dispatch_request_query_respects_cutoff(tmp_path):
    """Events before the cutoff are ignored."""
    log = tmp_path / "daemon.jsonl"
    _make_log(
        [
            {"beadId": "jleechan-stale", "lifecycleState": "ATTESTED", "timestamp": "2025-01-01T00:00:00Z"},
            {"beadId": "jleechan-fresh", "lifecycleState": "ATTESTED", "timestamp": "2026-08-07T00:00:00Z"},
        ],
        log,
    )
    bead_ids = _run_query("g11_dispatch_request", log, "2026-08-06T00:00:00Z")
    assert "jleechan-stale" not in bead_ids
    assert "jleechan-fresh" in bead_ids


def test_fe_audit_state_file_persists_last_sweep_snapshot(tmp_path, monkeypatch):
    """The audit state file must persist last_sweep_attested and
    last_sweep_dispatched so the next sweep can compute the delta.

    Without this, every sweep behaves like a "first run" — there is no
    snapshot to compare against, so no new ATTESTED rows can be detected
    and no DISPATCH_REQUEST events can be enqueued. Restart cycles thus
    strand beads in ATTESTED.
    """
    # Build a minimal daemon.jsonl that contains one ATTESTED bead.
    log = tmp_path / "daemon.jsonl"
    _make_log(
        [
            {"beadId": "jleechan-vxi3", "lifecycleState": "ATTESTED", "timestamp": "2026-08-07T00:00:00Z"},
        ],
        log,
    )

    # State file location is parameterized by FE_AUDIT_STATE_DIR.
    state_dir = tmp_path / "state"
    state_file = state_dir / "last-fired.json"
    log_file = log

    env = {
        "FE_AUDIT_LOG": str(log_file),
        "FE_AUDIT_STATE_DIR": str(state_dir),
        "LOOKBACK_HOURS": "24",
        "PATH": "/usr/bin:/bin",
        "BR_DB": str(tmp_path / "beads.db"),
    }
    # Wipe BR_DB so `br` isn't actually invoked (the audit only uses br
    # when STUCK_COUNT>0; in this minimal scenario the ATTESTED bead
    # without DISPATCH follow-up DOES trigger the bead path, so we need
    # br to silently no-op). We point the audit at a writable empty DB
    # and tolerate the br-not-found error path.
    monkeypatch.setattr("sys.argv", ["fe-audit"])

    # First run: state file is empty, so the snapshot is fresh.
    proc = subprocess.run(
        ["bash", str(AUDIT_SH), "--no-bead"],
        env=env,
        capture_output=True,
        text=True,
    )
    # The audit must exit 0 even when bead finds nothing.
    assert proc.returncode == 0, f"audit failed: stdout={proc.stdout} stderr={proc.stderr}"

    # The state file MUST now contain last_sweep_attested and
    # last_sweep_dispatched; without these, the next sweep cannot
    # detect "new ATTESTED beads" and the G11 bug recurs.
    assert state_file.exists(), f"state file not created: {state_file}"
    state = json.loads(state_file.read_text())
    assert "last_sweep_attested" in state, (
        f"last_sweep_attested missing from state file; got: {state}"
    )
    assert "last_sweep_dispatched" in state, (
        f"last_sweep_dispatched missing from state file; got: {state}"
    )
    assert "jleechan-vxi3" in state["last_sweep_attested"], (
        f"vxi3 missing from last_sweep_attested; got: {state['last_sweep_attested']}"
    )


def test_fe_audit_emits_dispatch_request_event_for_new_attested_bead(tmp_path):
    """When a new ATTESTED bead appears between sweeps, the audit MUST
    emit a DISPATCH_REQUEST event to the telemetry log so the
    always-on auto-factory daemon picks it up on its next tick.

    Without this, the bead is stranded in ATTESTED with no worker spawn —
    the original G11 incident.
    """
    # First sweep: empty log, so the snapshot persists as empty.
    log = tmp_path / "daemon.jsonl"
    log.write_text("")
    state_dir = tmp_path / "state"
    state_file = state_dir / "last-fired.json"

    env = {
        "FE_AUDIT_LOG": str(log),
        "FE_AUDIT_STATE_DIR": str(state_dir),
        "LOOKBACK_HOURS": "24",
        "PATH": "/usr/bin:/bin",
        "BR_DB": str(tmp_path / "beads.db"),
    }

    proc = subprocess.run(
        ["bash", str(AUDIT_SH), "--no-bead"],
        env=env,
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"first sweep failed: {proc.stderr}"

    # Second sweep: a new ATTESTED bead appears. The audit must emit a
    # DISPATCH_REQUEST event for it.
    _make_log(
        [
            {"beadId": "jleechan-vxi3", "lifecycleState": "ATTESTED", "timestamp": "2026-08-07T00:00:00Z"},
        ],
        log,
    )

    # The audit appends DISPATCH_REQUEST events to the telemetry log
    # (the daemon.jsonl itself), so we read it back after the second run.
    proc = subprocess.run(
        ["bash", str(AUDIT_SH), "--no-bead"],
        env=env,
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"second sweep failed: {proc.stderr}"

    # The audit may use a separate side-channel log for DISPATCH_REQUEST
    # events (e.g. <log_dir>/dispatch_requests.jsonl) so the daemon's
    # tick loop can read it without re-parsing the full daemon.jsonl on
    # every tick. Either way, the audit MUST emit one DISPATCH_REQUEST
    # entry for jleechan-vxi3.
    dispatch_request_log = state_dir / "dispatch_requests.jsonl"
    assert dispatch_request_log.exists(), (
        f"DISPATCH_REQUEST log not created: {dispatch_request_log}"
    )

    requests = [
        json.loads(line) for line in dispatch_request_log.read_text().splitlines() if line.strip()
    ]
    matching = [r for r in requests if r.get("beadId") == "jleechan-vxi3"]
    assert matching, (
        f"no DISPATCH_REQUEST entry for jleechan-vxi3; got: {requests}"
    )
    assert matching[0]["eventType"] == "DISPATCH_REQUEST", (
        f"unexpected event type: {matching[0]}"
    )
