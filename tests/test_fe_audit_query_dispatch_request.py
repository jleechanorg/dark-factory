#!/usr/bin/env python3
"""fe_audit_query coverage for the G11 DISPATCH_REQUEST pairing.

Bead jleechan-7lom / G11 startup-intake-without-forced-dispatch: the
slow-tier intake sweep now emits a `DISPATCH_REQUEST` telemetry event
(`lifecycleState=DISPATCHED, eventType=DISPATCH_REQUEST`) for every
ATTESTED bead that grew beyond the previous tick's snapshot
(daemon/src/tick.rs::emit_dispatch_request_for_grown_attested). The G11
fe-audit (daemon/scripts/fe-audit.sh:153) subtracts the DISPATCHED-set
from the ATTESTED-set to detect "stuck" beads — so the audit must match
the new `DISPATCH_REQUEST` lifecycleState alongside the existing
`TASK_DISPATCHED` flow.

This test pins the contract at the python helper layer (the cheapest
place to assert the audit-side predicate) so a future refactor that
breaks the G11 audit's consumption of the new telemetry event fails
this test loudly instead of letting the false-positive G11 factory
beads silently resume.

Run: .venv/bin/python -m pytest tests/test_fe_audit_query_dispatch_request.py -v
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).parent.parent
QUERY_PY = ROOT / "daemon" / "scripts" / "fe_audit_query.py"


def run_query(query: str, records: list[dict], cutoff: str) -> list[str]:
    """Invoke fe_audit_query.py as a subprocess with a JSONL file."""
    import tempfile
    with tempfile.NamedTemporaryFile(mode="w", suffix=".jsonl", delete=False) as f:
        for r in records:
            f.write(json.dumps(r) + "\n")
        log_path = f.name
    try:
        out = subprocess_run([sys.executable, str(QUERY_PY), query, log_path, cutoff])
        return [line for line in out.splitlines() if line]
    finally:
        Path(log_path).unlink(missing_ok=True)


def subprocess_run(cmd):
    import subprocess
    result = subprocess.run(cmd, capture_output=True, text=True, check=True)
    return result.stdout


def test_g11_dispatched_matches_dispatch_request_event():
    """A DISPATCH_REQUEST event (lifecycleState=DISPATCHED) must count
    toward the g11_dispatched set so the G11 audit subtracts it from the
    ATTESTED-stuck set."""
    records = [
        {
            "timestamp": "2026-08-01T12:00:00Z",
            "beadId": "jleechan-stuck-bead",
            "lifecycleState": "ATTESTED",
            "eventType": "STATE_TRANSITION",
        },
        {
            "timestamp": "2026-08-01T12:05:00Z",
            "beadId": "jleechan-stuck-bead",
            "lifecycleState": "DISPATCHED",
            "eventType": "DISPATCH_REQUEST",
        },
    ]
    dispatched = run_query("g11_dispatched", records, "2026-08-01T00:00:00Z")
    assert "jleechan-stuck-bead" in dispatched, (
        f"DISPATCH_REQUEST must match g11_dispatched; got {dispatched}"
    )


def test_g11_dispatched_matches_task_dispatched_event():
    """The pre-existing TASK_DISPATCHED flow still matches — no
    regression of the original G11 audit pairing."""
    records = [
        {
            "timestamp": "2026-08-01T12:00:00Z",
            "beadId": "jleechan-task-dispatched",
            "lifecycleState": "DISPATCHED",
            "eventType": "TASK_DISPATCHED",
        },
    ]
    dispatched = run_query("g11_dispatched", records, "2026-08-01T00:00:00Z")
    assert "jleechan-task-dispatched" in dispatched, (
        f"TASK_DISPATCHED must continue to match g11_dispatched; got {dispatched}"
    )


def test_g11_dispatched_respects_cutoff():
    """DISPATCH_REQUEST events older than the cutoff must NOT be counted."""
    records = [
        {
            "timestamp": "2025-01-01T00:00:00Z",  # ancient — pre-cutoff
            "beadId": "jleechan-old",
            "lifecycleState": "DISPATCHED",
            "eventType": "DISPATCH_REQUEST",
        },
        {
            "timestamp": "2026-08-01T12:00:00Z",
            "beadId": "jleechan-new",
            "lifecycleState": "DISPATCHED",
            "eventType": "DISPATCH_REQUEST",
        },
    ]
    dispatched = run_query("g11_dispatched", records, "2026-08-01T00:00:00Z")
    assert "jleechan-new" in dispatched
    assert "jleechan-old" not in dispatched, (
        f"pre-cutoff DISPATCH_REQUEST must not match; got {dispatched}"
    )


def test_g11_dispatched_dedupes_repeated_emissions():
    """Multiple DISPATCH_REQUEST events for the same bead must dedupe
    to a single entry in the g11_dispatched set (the audit's `set`
    semantics). The python helper returns sorted unique bead IDs."""
    records = [
        {
            "timestamp": "2026-08-01T12:00:00Z",
            "beadId": "jleechan-twice",
            "lifecycleState": "DISPATCHED",
            "eventType": "DISPATCH_REQUEST",
        },
        {
            "timestamp": "2026-08-01T13:00:00Z",
            "beadId": "jleechan-twice",
            "lifecycleState": "DISPATCHED",
            "eventType": "DISPATCH_REQUEST",
        },
    ]
    dispatched = run_query("g11_dispatched", records, "2026-08-01T00:00:00Z")
    assert dispatched.count("jleechan-twice") == 1, (
        f"duplicate DISPATCH_REQUEST events must dedupe; got {dispatched}"
    )
