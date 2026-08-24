"""Tests for the df-funnel bead-lifecycle throughput report (bead rev-2vqpa).

Fixture design — five synthetic (bead_id, attempt_id) lifecycles written as
a daemon.jsonl file, with hand-verified expected stage counts / conversion
percentages / latencies (see the comment block above ``FIXTURE_ROWS``):

  A (rev-aaaa, attempt 1): full happy path, TASK_DISPATCHED -> PR_OPENED ->
    GATE_ASSESSMENT -> READY_FOR_MERGE.
  B (rev-bbbb, attempt 1): full happy path, different latencies.
  C (rev-cccc, attempt 1): dispatched + PR opened, then PARKED_HUMAN_HELD
    before a gate assessment ever landed.
  D (rev-dddd, attempt 1): dispatched, then ESCALATION_REQUIRED — never
    even opened a PR.
  E (rev-eeee, attempt 2): dispatched, PR opened, gate-assessed, then
    PARKED_HUMAN_HELD instead of reaching READY_FOR_MERGE.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
from datetime import datetime, timedelta, timezone

import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

from runner.funnel_report import (
    compute_funnel,
    load_events,
    main,
    parse_since,
    render_json,
    render_markdown,
)

T0 = datetime(2026, 1, 1, 0, 0, 0, tzinfo=timezone.utc)


def _row(bead_id, attempt, event_type, offset_s, base=T0, **schema_kwargs):
    """Build one daemon.jsonl row. Defaults to the CURRENT schema
    (timestamp/attemptId); pass schema_kwargs={'legacy': True} to emit the
    OLD schema (ts/attempt) instead, to prove cross-schema tolerance."""
    ts = (base + timedelta(seconds=offset_s)).strftime("%Y-%m-%dT%H:%M:%SZ")
    if schema_kwargs.get("legacy"):
        return {
            "ts": ts,
            "beadId": bead_id,
            "attempt": attempt,
            "state": "DISPATCHED",
            "eventType": event_type,
            "counts": {},
            "context": {},
        }
    return {
        "timestamp": ts,
        "beadId": bead_id,
        "attemptId": attempt,
        "lifecycleState": "DISPATCHED",
        "eventType": event_type,
        "metrics": {},
        "context": {},
    }


def _build_rows(base):
    """Five synthetic lifecycles anchored at ``base`` (see module docstring
    for the A-E scenario descriptions and the hand-verified stage deltas
    referenced in the tests below)."""
    return [
        # A: full happy path. TASK_DISPATCHED->PR_OPENED delta=60s,
        #    PR_OPENED->GATE_ASSESSMENT delta=240s, GATE_ASSESSMENT->READY delta=600s.
        _row("rev-aaaa", 1, "TASK_DISPATCHED", 0, base=base),
        _row("rev-aaaa", 1, "PR_OPENED", 60, base=base),
        _row("rev-aaaa", 1, "GATE_ASSESSMENT", 300, base=base),
        _row("rev-aaaa", 1, "READY_FOR_MERGE", 900, base=base),
        # B: full happy path, legacy schema fields (ts/attempt instead of
        #    timestamp/attemptId) to prove cross-schema tolerance.
        #    deltas: dispatch->pr=120s, pr->gate=300s, gate->ready=600s.
        _row("rev-bbbb", 1, "TASK_DISPATCHED", 10, base=base, legacy=True),
        _row("rev-bbbb", 1, "PR_OPENED", 130, base=base, legacy=True),
        _row("rev-bbbb", 1, "GATE_ASSESSMENT", 430, base=base, legacy=True),
        _row("rev-bbbb", 1, "READY_FOR_MERGE", 1030, base=base, legacy=True),
        # C: dispatched + PR opened (delta=240s), then parked before any gate
        #    assessment ever landed.
        _row("rev-cccc", 1, "TASK_DISPATCHED", 20, base=base),
        _row("rev-cccc", 1, "PR_OPENED", 260, base=base),
        _row("rev-cccc", 1, "PARKED_HUMAN_HELD", 500, base=base),
        # D: dispatched, then escalated straight away — never opened a PR.
        _row("rev-dddd", 1, "TASK_DISPATCHED", 30, base=base),
        _row("rev-dddd", 1, "ESCALATION_REQUIRED", 200, base=base),
        # E: dispatched, PR opened (delta=180s), gate-assessed (delta=330s),
        #    then parked instead of reaching READY_FOR_MERGE. Distinct attempt
        #    id (2) on purpose, to prove grouping is (bead_id, attempt_id).
        _row("rev-eeee", 2, "TASK_DISPATCHED", 40, base=base),
        _row("rev-eeee", 2, "PR_OPENED", 220, base=base),
        _row("rev-eeee", 2, "GATE_ASSESSMENT", 550, base=base),
        _row("rev-eeee", 2, "PARKED_HUMAN_HELD", 900, base=base),
    ]


# Fixed-clock fixture (anchored to T0) for tests that pass an explicit
# ``now=`` to load_events / don't filter by --since at all.
FIXTURE_ROWS = _build_rows(T0)


def _write_fixture(tmp_path, rows=None, extra_lines=None):
    path = tmp_path / "daemon.jsonl"
    lines = [json.dumps(r) for r in (rows if rows is not None else FIXTURE_ROWS)]
    if extra_lines:
        lines = extra_lines + lines
    path.write_text("\n".join(lines) + "\n")
    return path


def _write_recent_fixture(tmp_path):
    """Same five lifecycles, anchored just under the real wall clock so a
    real ``--since 48h`` CLI run (which uses ``datetime.now()`` internally)
    includes them regardless of when the test suite executes."""
    base = datetime.now(timezone.utc) - timedelta(hours=1)
    return _write_fixture(tmp_path, rows=_build_rows(base))


# ---------------------------------------------------------------------------
# parse_since
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "value,expected",
    [
        ("48h", timedelta(hours=48)),
        ("7d", timedelta(days=7)),
        ("30m", timedelta(minutes=30)),
        ("90s", timedelta(seconds=90)),
        ("2D", timedelta(days=2)),
    ],
)
def test_parse_since_valid(value, expected):
    assert parse_since(value) == expected


@pytest.mark.parametrize("value", ["", "48", "h48", "48x", "abc"])
def test_parse_since_rejects_invalid(value):
    with pytest.raises(ValueError):
        parse_since(value)


# ---------------------------------------------------------------------------
# load_events — parsing, schema tolerance, malformed-line skipping, --since
# ---------------------------------------------------------------------------


def test_load_events_parses_fixture(tmp_path):
    path = _write_fixture(tmp_path)
    events = load_events(path)
    assert len(events) == len(FIXTURE_ROWS)
    # Legacy-schema row (rev-bbbb) must normalize identically to the
    # current-schema rows: same keys, correct types.
    bbbb = [e for e in events if e["bead_id"] == "rev-bbbb"]
    assert len(bbbb) == 4
    assert all(e["attempt_id"] == 1 for e in bbbb)
    assert all(isinstance(e["ts"], datetime) for e in bbbb)


def test_load_events_skips_malformed_and_incomplete_lines(tmp_path):
    extra = [
        "not json at all {{{",
        json.dumps({"eventType": "TASK_DISPATCHED"}),  # missing beadId/ts
        json.dumps({"beadId": "rev-x", "eventType": "TASK_DISPATCHED"}),  # missing ts
        "",  # blank line
    ]
    path = _write_fixture(tmp_path, extra_lines=extra)
    events = load_events(path)
    # Only the well-formed fixture rows should survive.
    assert len(events) == len(FIXTURE_ROWS)


def test_load_events_since_window_excludes_old_rows(tmp_path):
    old_row = _row("rev-old", 1, "TASK_DISPATCHED", -1_000_000)  # far in the past
    path = _write_fixture(tmp_path, rows=[old_row] + FIXTURE_ROWS)
    now = T0 + timedelta(seconds=2000)
    events = load_events(path, since=timedelta(hours=48), now=now)
    bead_ids = {e["bead_id"] for e in events}
    assert "rev-old" not in bead_ids
    assert len(events) == len(FIXTURE_ROWS)


# ---------------------------------------------------------------------------
# compute_funnel — hand-verified counts / conversion / latency
# ---------------------------------------------------------------------------


def test_compute_funnel_stage_counts_and_conversion(tmp_path):
    path = _write_fixture(tmp_path)
    events = load_events(path)
    report = compute_funnel(events, since_label="48h")

    assert report.total_lifecycles == 5  # A, B, C, D, E

    by_stage = {s.stage: s for s in report.main_stages}
    assert by_stage["TASK_DISPATCHED"].count == 5
    assert by_stage["TASK_DISPATCHED"].conversion_pct is None

    assert by_stage["PR_OPENED"].count == 4  # everyone but D
    assert by_stage["PR_OPENED"].conversion_pct == pytest.approx(80.0)

    assert by_stage["GATE_ASSESSMENT"].count == 3  # A, B, E
    assert by_stage["GATE_ASSESSMENT"].conversion_pct == pytest.approx(75.0)

    assert by_stage["READY_FOR_MERGE"].count == 2  # A, B only
    assert by_stage["READY_FOR_MERGE"].conversion_pct == pytest.approx(200.0 / 3.0)

    by_side = {s.stage: s for s in report.side_stages}
    assert by_side["PARKED_HUMAN_HELD"].count == 2  # C, E
    assert by_side["PARKED_HUMAN_HELD"].pct_of_dispatched == pytest.approx(40.0)
    assert by_side["ESCALATION_REQUIRED"].count == 1  # D
    assert by_side["ESCALATION_REQUIRED"].pct_of_dispatched == pytest.approx(20.0)


def test_compute_funnel_latency_percentiles(tmp_path):
    path = _write_fixture(tmp_path)
    events = load_events(path)
    report = compute_funnel(events)
    by_stage = {s.stage: s for s in report.main_stages}

    # dispatch->PR deltas across A,B,C,E: [60, 120, 240, 180] -> sorted [60,120,180,240]
    pr = by_stage["PR_OPENED"]
    assert pr.latency_p50_s == pytest.approx(150.0)
    assert pr.latency_p95_s == pytest.approx(231.0)

    # PR->gate deltas across A,B,E: [240, 300, 330] -> sorted as-is
    gate = by_stage["GATE_ASSESSMENT"]
    assert gate.latency_p50_s == pytest.approx(300.0)
    assert gate.latency_p95_s == pytest.approx(327.0)

    # gate->ready deltas across A,B only: [600, 600]
    ready = by_stage["READY_FOR_MERGE"]
    assert ready.latency_p50_s == pytest.approx(600.0)
    assert ready.latency_p95_s == pytest.approx(600.0)


def test_compute_funnel_empty_events_produces_zeroed_report():
    report = compute_funnel([])
    assert report.total_lifecycles == 0
    for s in report.main_stages:
        assert s.count == 0
    for s in report.side_stages:
        assert s.count == 0
        assert s.pct_of_dispatched is None


# ---------------------------------------------------------------------------
# render_markdown / render_json
# ---------------------------------------------------------------------------


def test_render_markdown_contains_stage_rows(tmp_path):
    path = _write_fixture(tmp_path)
    report = compute_funnel(load_events(path), since_label="48h")
    text = render_markdown(report)
    assert "TASK_DISPATCHED" in text
    assert "READY_FOR_MERGE" in text
    assert "PARKED_HUMAN_HELD" in text
    assert "ESCALATION_REQUIRED" in text
    assert "80.0%" in text  # PR_OPENED conversion


def test_render_json_round_trips_via_json_loads(tmp_path):
    path = _write_fixture(tmp_path)
    report = compute_funnel(load_events(path), since_label="48h")
    payload = render_json(report)
    dumped = json.dumps(payload)
    parsed = json.loads(dumped)
    assert parsed["total_lifecycles"] == 5
    stage_names = [s["stage"] for s in parsed["main_stages"]]
    assert stage_names == ["TASK_DISPATCHED", "PR_OPENED", "GATE_ASSESSMENT", "READY_FOR_MERGE"]
    ready = next(s for s in parsed["main_stages"] if s["stage"] == "READY_FOR_MERGE")
    assert ready["count"] == 2


# ---------------------------------------------------------------------------
# CLI (main())
# ---------------------------------------------------------------------------


def test_cli_main_markdown_output(tmp_path, capsys):
    path = _write_recent_fixture(tmp_path)
    rc = main(["--daemon-log", str(path), "--since", "48h"])
    assert rc == 0
    out = capsys.readouterr().out
    assert "df-funnel report" in out
    assert "READY_FOR_MERGE" in out


def test_cli_main_json_output_is_valid_json(tmp_path, capsys):
    path = _write_recent_fixture(tmp_path)
    rc = main(["--daemon-log", str(path), "--since", "48h", "--json"])
    assert rc == 0
    out = capsys.readouterr().out
    parsed = json.loads(out)
    assert parsed["total_lifecycles"] == 5


def test_cli_main_missing_log_returns_error(tmp_path, capsys):
    missing = tmp_path / "does-not-exist.jsonl"
    rc = main(["--daemon-log", str(missing)])
    assert rc == 1
    err = capsys.readouterr().err
    assert "not found" in err


def test_cli_main_invalid_since_returns_error(tmp_path, capsys):
    path = _write_fixture(tmp_path)
    rc = main(["--daemon-log", str(path), "--since", "notaduration"])
    assert rc == 2


def test_cli_subprocess_module_invocation(tmp_path):
    """Smoke-test the real console entry point path (python -m runner.funnel_report),
    matching how bin/df-funnel invokes it."""
    path = _write_recent_fixture(tmp_path)
    result = subprocess.run(
        [sys.executable, "-m", "runner.funnel_report", "--daemon-log", str(path), "--since", "48h"],
        capture_output=True,
        text=True,
        check=False,
        cwd=str(REPO_ROOT),
    )
    assert result.returncode == 0
    assert "df-funnel report" in result.stdout
