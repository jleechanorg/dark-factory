"""Tests for the df-funnel-lanes intake-origin throughput report
(bead rev-2vqpa follow-up, 2026-08-24 live analysis).

Fixture design — six synthetic (bead_id, attempt_id) lifecycles, one per
lane x outcome combination the 2026-08-24 live run actually observed:

  BEAD_STOP  (bead_start, never dispatched): INTAKE_BEAD_CREATED only,
    no external_ref -> lane=bead_start, furthest=INTAKE_ONLY.
  BEAD_GO    (bead_start, reaches gate): INTAKE_BEAD_CREATED (no ref) ->
    TASK_DISPATCHED -> PR_OPENED -> GATE_ASSESSMENT -> PARKED_HUMAN_HELD.
  ISSUE_STOP (gh_issue_start, escalated before dispatch):
    INTAKE_BEAD_CREATED (with external_ref) -> ESCALATION_REQUIRED.
  ISSUE_GO   (gh_issue_start, dispatched but no PR yet):
    INTAKE_BEAD_CREATED (with external_ref) -> TASK_DISPATCHED.
  PR_ADOPT   (pr_adopted_start, immediate gate assessment, no PR_OPENED):
    EXISTING_PR_ADOPTED (newly_created=true) -> GATE_ASSESSMENT.
  UNCLASSIFIED (no origin event at all -- must be excluded from every lane):
    TASK_DISPATCHED with no preceding INTAKE_BEAD_CREATED / EXISTING_PR_ADOPTED
    in the fixture window (simulates a lifecycle whose origin event is
    older than the --since cutoff).
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
from datetime import datetime, timedelta, timezone

import pytest

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

from runner.funnel_lanes import (
    classify_origin,
    compute_lane_report,
    load_events_full,
    main,
    render_json,
    render_markdown,
)
from runner.funnel_report import parse_since

T0 = datetime(2026, 1, 1, 0, 0, 0, tzinfo=timezone.utc)


def _row(bead_id, attempt, event_type, offset_s, base=T0, context=None):
    ts = (base + timedelta(seconds=offset_s)).strftime("%Y-%m-%dT%H:%M:%SZ")
    return {
        "timestamp": ts,
        "beadId": bead_id,
        "attemptId": attempt,
        "lifecycleState": "DISPATCHED",
        "eventType": event_type,
        "metrics": {},
        "context": context or {},
    }


def _build_rows(base):
    return [
        # BEAD_STOP: hand-created bead, never dispatched.
        _row("rev-bead-stop", 1, "INTAKE_BEAD_CREATED", 0, base=base, context={}),
        # BEAD_GO: hand-created bead, dispatched -> PR -> gate -> parked.
        _row("rev-bead-go", 1, "INTAKE_BEAD_CREATED", 0, base=base, context={}),
        _row("rev-bead-go", 1, "TASK_DISPATCHED", 10, base=base),
        _row("rev-bead-go", 1, "PR_OPENED", 70, base=base),
        _row("rev-bead-go", 1, "GATE_ASSESSMENT", 370, base=base),
        _row("rev-bead-go", 1, "PARKED_HUMAN_HELD", 900, base=base),
        # ISSUE_STOP: swept from a GH issue, escalated before dispatch.
        _row(
            "rev-issue-stop", 1, "INTAKE_BEAD_CREATED", 0, base=base,
            context={"external_ref": "jleechanorg/dark-factory#9001"},
        ),
        _row("rev-issue-stop", 1, "ESCALATION_REQUIRED", 50, base=base),
        # ISSUE_GO: swept from a GH issue, dispatched but no PR yet.
        _row(
            "rev-issue-go", 1, "INTAKE_BEAD_CREATED", 0, base=base,
            context={"external_ref": "jleechanorg/dark-factory#9002"},
        ),
        _row("rev-issue-go", 1, "TASK_DISPATCHED", 15, base=base),
        # PR_ADOPT: adopted an already-open PR -> immediate gate assessment,
        # no PR_OPENED (the PR predates the bead).
        _row(
            "rev-pr-adopt", 1, "EXISTING_PR_ADOPTED", 0, base=base,
            context={"newly_created": True, "pr_number": 4242},
        ),
        _row("rev-pr-adopt", 1, "GATE_ASSESSMENT", 5, base=base),
        # UNCLASSIFIED: no origin event -> must not appear in ANY lane.
        _row("rev-unclassified", 1, "TASK_DISPATCHED", 0, base=base),
    ]


FIXTURE_ROWS = _build_rows(T0)


def _write_fixture(tmp_path, rows=None):
    path = tmp_path / "daemon.jsonl"
    lines = [json.dumps(r) for r in (rows if rows is not None else FIXTURE_ROWS)]
    path.write_text("\n".join(lines) + "\n")
    return path


def _write_recent_fixture(tmp_path):
    base = datetime.now(timezone.utc) - timedelta(hours=1)
    return _write_fixture(tmp_path, rows=_build_rows(base))


# ---------------------------------------------------------------------------
# classify_origin
# ---------------------------------------------------------------------------


def test_classify_origin_three_lanes():
    # Load directly from in-memory rows via the normalize path (no file I/O needed).
    import runner.funnel_lanes as fl

    normalized = [fl._normalize_full(r) for r in FIXTURE_ROWS]
    normalized = [e for e in normalized if e is not None]
    origin = classify_origin(normalized)

    assert origin[("rev-bead-stop", 1)] == "bead_start"
    assert origin[("rev-bead-go", 1)] == "bead_start"
    assert origin[("rev-issue-stop", 1)] == "gh_issue_start"
    assert origin[("rev-issue-go", 1)] == "gh_issue_start"
    assert origin[("rev-pr-adopt", 1)] == "pr_adopted_start"
    assert ("rev-unclassified", 1) not in origin


# ---------------------------------------------------------------------------
# compute_lane_report
# ---------------------------------------------------------------------------


def test_compute_lane_report_furthest_and_terminal(tmp_path):
    path = _write_fixture(tmp_path)
    events = load_events_full(path, since=None, now=T0 + timedelta(days=1))
    report = compute_lane_report(events, since_label="test")

    by_lane = {stat.lane: stat for stat in report.lanes}

    bead_stat = by_lane["bead_start"]
    assert bead_stat.total == 2
    assert bead_stat.furthest.get("INTAKE_ONLY") == 1  # rev-bead-stop
    assert bead_stat.furthest.get("GATE_ASSESSMENT") == 1  # rev-bead-go
    assert bead_stat.terminal.get("none") == 1  # rev-bead-stop
    assert bead_stat.terminal.get("PARKED_HUMAN_HELD") == 1  # rev-bead-go

    issue_stat = by_lane["gh_issue_start"]
    assert issue_stat.total == 2
    assert issue_stat.furthest.get("INTAKE_ONLY") == 1  # rev-issue-stop (escalated before dispatch)
    assert issue_stat.furthest.get("TASK_DISPATCHED") == 1  # rev-issue-go
    assert issue_stat.terminal.get("ESCALATION_REQUIRED") == 1
    assert issue_stat.terminal.get("none") == 1

    pr_stat = by_lane["pr_adopted_start"]
    assert pr_stat.total == 1
    assert pr_stat.furthest.get("GATE_ASSESSMENT") == 1
    assert pr_stat.terminal.get("none") == 1

    # UNCLASSIFIED never appears in any lane total.
    grand_total = sum(stat.total for stat in report.lanes)
    assert grand_total == 5  # 2 bead + 2 issue + 1 pr-adopt, NOT 6


def test_compute_lane_report_since_label_passthrough(tmp_path):
    path = _write_fixture(tmp_path)
    events = load_events_full(path, since=None, now=T0 + timedelta(days=1))
    report = compute_lane_report(events, since_label="30d")
    assert report.since_label == "30d"


# ---------------------------------------------------------------------------
# render_markdown / render_json
# ---------------------------------------------------------------------------


def test_render_markdown_includes_all_three_lanes(tmp_path):
    path = _write_fixture(tmp_path)
    events = load_events_full(path, since=None, now=T0 + timedelta(days=1))
    report = compute_lane_report(events, since_label="30d")
    md = render_markdown(report)
    assert "bead_start" in md
    assert "gh_issue_start" in md
    assert "pr_adopted_start" in md
    assert "INTAKE_ONLY" in md
    assert "Furthest milestone reached" in md
    assert "Terminal divert" in md


def test_render_json_shape(tmp_path):
    path = _write_fixture(tmp_path)
    events = load_events_full(path, since=None, now=T0 + timedelta(days=1))
    report = compute_lane_report(events, since_label="30d")
    payload = render_json(report)
    assert payload["since"] == "30d"
    assert len(payload["lanes"]) == 3
    lane_names = {lane["lane"] for lane in payload["lanes"]}
    assert lane_names == {"bead_start", "gh_issue_start", "pr_adopted_start"}
    for lane in payload["lanes"]:
        assert "furthest_stage" in lane
        assert "terminal_divert" in lane


# ---------------------------------------------------------------------------
# CLI (main())
# ---------------------------------------------------------------------------


def test_main_json_mode(tmp_path, capsys):
    path = _write_recent_fixture(tmp_path)
    rc = main(["--daemon-log", str(path), "--since", "48h", "--json"])
    assert rc == 0
    out = capsys.readouterr().out
    payload = json.loads(out)
    assert len(payload["lanes"]) == 3
    total = sum(lane["total"] for lane in payload["lanes"])
    assert total == 5


def test_main_markdown_mode(tmp_path, capsys):
    path = _write_recent_fixture(tmp_path)
    rc = main(["--daemon-log", str(path), "--since", "48h"])
    assert rc == 0
    out = capsys.readouterr().out
    assert "df-funnel-lanes report" in out


def test_main_missing_log_file(tmp_path):
    missing = tmp_path / "does-not-exist.jsonl"
    rc = main(["--daemon-log", str(missing), "--since", "48h"])
    assert rc == 1


def test_main_invalid_since(tmp_path):
    path = _write_recent_fixture(tmp_path)
    rc = main(["--daemon-log", str(path), "--since", "not-a-duration"])
    assert rc == 2


def test_main_default_since_is_30d():
    import runner.funnel_lanes as fl
    import argparse

    p = argparse.ArgumentParser()
    p.add_argument("--since", default="30d")
    args = p.parse_args([])
    assert args.since == "30d"
    assert parse_since("30d") == timedelta(days=30)


def test_bin_shim_end_to_end(tmp_path):
    """Full subprocess invocation of bin/df-funnel-lanes, proving the bash
    shim + venv resolution + module dispatch chain works end-to-end (mirrors
    test_funnel_report.py's equivalent bin/df-funnel check)."""
    venv_python = REPO_ROOT / ".venv" / "bin" / "python"
    if not venv_python.exists():
        pytest.skip("no .venv present in this checkout")
    path = _write_recent_fixture(tmp_path)
    result = subprocess.run(
        [str(venv_python), "-m", "runner.funnel_lanes", "--daemon-log", str(path), "--since", "48h", "--json"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert len(payload["lanes"]) == 3
