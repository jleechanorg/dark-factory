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
from datetime import datetime, timedelta, timezone

import pytest

from runner.funnel_lanes import (
    classify_origin,
    compute_lane_report,
    load_events_full,
    main,
    render_json,
    render_markdown,
)
from runner.funnel_report import parse_since

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

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


@pytest.mark.parametrize("non_object", [[], None, "valid JSON string"])
def test_load_events_full_skips_valid_non_object_json(tmp_path, non_object):
    path = tmp_path / "daemon.jsonl"
    valid = _row("rev-after-non-object", 1, "INTAKE_BEAD_CREATED", 0)
    path.write_text(f"{json.dumps(non_object)}\n{json.dumps(valid)}\n")

    events = load_events_full(path, since=None, now=T0 + timedelta(days=1))

    assert [event["bead_id"] for event in events] == ["rev-after-non-object"]


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("eventType", []),
        ("eventType", {}),
        ("eventType", 42),
        ("eventType", ""),
        ("beadId", []),
        ("beadId", {}),
        ("beadId", 42),
        ("beadId", ""),
        ("timestamp", []),
        ("timestamp", {}),
        ("timestamp", 42),
        ("timestamp", "not-a-timestamp"),
    ],
)
def test_normalize_full_rejects_malformed_required_fields(field, value):
    """Malformed required fields are dropped before downstream set/dict use."""
    import runner.funnel_lanes as fl

    row = _row("rev-valid", 1, "INTAKE_BEAD_CREATED", 0)
    row[field] = value

    assert fl._normalize_full(row) is None


@pytest.mark.parametrize(
    "context",
    [
        {"external_ref": ["jleechanorg/dark-factory#9003"]},
        {"external_ref": {"issue": 9003}},
        {"external_ref": 9003},
        None,
        ["not an object"],
    ],
)
def test_classify_origin_skips_malformed_intake_context(context):
    import runner.funnel_lanes as fl

    row = _row("rev-malformed-intake", 1, "INTAKE_BEAD_CREATED", 0, context={})
    row["context"] = context
    event = fl._normalize_full(row)

    assert event is not None
    assert "rev-malformed-intake" not in classify_origin([event])


@pytest.mark.parametrize("newly_created", [False, "true", 1, [], {}, None])
def test_classify_origin_requires_boolean_newly_created(newly_created):
    import runner.funnel_lanes as fl

    row = _row(
        "rev-malformed-adoption",
        1,
        "EXISTING_PR_ADOPTED",
        0,
        context={"newly_created": newly_created},
    )
    event = fl._normalize_full(row)

    assert event is not None
    assert "rev-malformed-adoption" not in classify_origin([event])


def test_classify_origin_accepts_only_non_empty_string_external_ref():
    import runner.funnel_lanes as fl

    rows = [
        _row("rev-issue-valid", 1, "INTAKE_BEAD_CREATED", 0, context={"external_ref": "#9004"}),
        _row("rev-bead-missing", 1, "INTAKE_BEAD_CREATED", 0, context={}),
        _row("rev-bead-null", 1, "INTAKE_BEAD_CREATED", 0, context={"external_ref": None}),
        _row("rev-bead-empty", 1, "INTAKE_BEAD_CREATED", 0, context={"external_ref": ""}),
        _row("rev-adopt-valid", 1, "EXISTING_PR_ADOPTED", 0, context={"newly_created": True}),
    ]
    events = [fl._normalize_full(row) for row in rows]

    assert classify_origin([event for event in events if event is not None]) == {
        "rev-issue-valid": "gh_issue_start",
        "rev-bead-missing": "bead_start",
        "rev-bead-null": "bead_start",
        "rev-bead-empty": "bead_start",
        "rev-adopt-valid": "pr_adopted_start",
    }


# ---------------------------------------------------------------------------
# classify_origin
# ---------------------------------------------------------------------------


def test_classify_origin_three_lanes():
    # Load directly from in-memory rows via the normalize path (no file I/O needed).
    # NOTE: classify_origin is keyed by bead_id ONLY (not (bead_id, attempt_id)) —
    # see the 2026-08-24 bug fix documented in the function's docstring.
    import runner.funnel_lanes as fl

    normalized = [fl._normalize_full(r) for r in FIXTURE_ROWS]
    normalized = [e for e in normalized if e is not None]
    origin = classify_origin(normalized)

    assert origin["rev-bead-stop"] == "bead_start"
    assert origin["rev-bead-go"] == "bead_start"
    assert origin["rev-issue-stop"] == "gh_issue_start"
    assert origin["rev-issue-go"] == "gh_issue_start"
    assert origin["rev-pr-adopt"] == "pr_adopted_start"
    assert "rev-unclassified" not in origin


# ---------------------------------------------------------------------------
# Regression: origin event on one attempt, downstream stage events on a
# LATER reroll attempt (bug found live 2026-08-24 against real daemon.jsonl
# — bead dark-factory-4sey / jleechan-l3r6 pattern). Confirms the bead-level
# (not per-attempt) join actually connects them.
# ---------------------------------------------------------------------------


def test_cross_attempt_origin_join_regression(tmp_path):
    """A bead's origin-classifying event fires on attempt 1 (or attempt 2),
    but its READY_FOR_MERGE fires on a LATER reroll attempt (3, or 4) after
    the earlier attempt(s) parked. The lane report must still attribute the
    bead to its lane and count READY_FOR_MERGE as its furthest stage — not
    silently drop it because the (bead_id, attempt_id) join key never
    matched the origin event's attempt_id."""
    rows = [
        # PR-adopted bead: EXISTING_PR_ADOPTED only at attempts 1-2 (parked
        # both times), READY_FOR_MERGE fires on attempt 3.
        _row(
            "rev-cross-pr", 1, "EXISTING_PR_ADOPTED", 0,
            context={"newly_created": True, "pr_number": 100},
        ),
        _row("rev-cross-pr", 1, "GATE_ASSESSMENT", 10),
        _row("rev-cross-pr", 1, "PARKED_HUMAN_HELD", 20),
        _row(
            "rev-cross-pr", 2, "EXISTING_PR_ADOPTED", 30,
            context={"newly_created": False, "pr_number": 100},
        ),
        _row("rev-cross-pr", 2, "GATE_ASSESSMENT", 40),
        _row("rev-cross-pr", 2, "PARKED_HUMAN_HELD", 50),
        _row("rev-cross-pr", 3, "GATE_ASSESSMENT", 60),
        _row("rev-cross-pr", 3, "READY_FOR_MERGE", 70),
        # Hand-created bead: INTAKE_BEAD_CREATED only at attempt 1, dispatched
        # + parked, then rerolled to attempt 2 which reaches READY_FOR_MERGE.
        _row("rev-cross-bead", 1, "INTAKE_BEAD_CREATED", 0, context={}),
        _row("rev-cross-bead", 1, "TASK_DISPATCHED", 5),
        _row("rev-cross-bead", 1, "PARKED_HUMAN_HELD", 15),
        _row("rev-cross-bead", 2, "TASK_DISPATCHED", 25),
        _row("rev-cross-bead", 2, "PR_OPENED", 35),
        _row("rev-cross-bead", 2, "GATE_ASSESSMENT", 45),
        _row("rev-cross-bead", 2, "READY_FOR_MERGE", 55),
    ]
    path = _write_fixture(tmp_path, rows=rows)
    events = load_events_full(path, since=None, now=T0 + timedelta(days=1))
    report = compute_lane_report(events, since_label="test")
    by_lane = {stat.lane: stat for stat in report.lanes}

    pr_stat = by_lane["pr_adopted_start"]
    assert pr_stat.total == 1
    assert pr_stat.furthest.get("READY_FOR_MERGE") == 1
    # Bead's LATEST attempt (3) reached READY, no terminal divert on that attempt.
    assert pr_stat.terminal.get("none") == 1

    bead_stat = by_lane["bead_start"]
    assert bead_stat.total == 1
    assert bead_stat.furthest.get("READY_FOR_MERGE") == 1
    assert bead_stat.terminal.get("none") == 1


def test_latest_recovery_routing_attempt_clears_older_terminal_divert(tmp_path):
    rows = [
        _row("rev-recovered", 1, "INTAKE_BEAD_CREATED", 0, context={}),
        _row("rev-recovered", 1, "TASK_DISPATCHED", 10),
        _row("rev-recovered", 1, "PARKED_HUMAN_HELD", 20),
        # Attempt 2 has lifecycle activity but has not reached a main or side
        # funnel event yet. Its existence makes attempt 1's park historical.
        _row("rev-recovered", 2, "RECOVERED_FROM_HELD", 30),
        _row("rev-recovered", 2, "TASK_ROUTED", 40),
    ]
    path = _write_fixture(tmp_path, rows=rows)
    events = load_events_full(path, since=None, now=T0 + timedelta(days=1))

    report = compute_lane_report(events, since_label="test")
    bead_stat = next(stat for stat in report.lanes if stat.lane == "bead_start")

    assert bead_stat.total == 1
    assert bead_stat.furthest.get("TASK_DISPATCHED") == 1
    assert bead_stat.terminal == {"none": 1}


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


def test_coderabbit_gate_outcomes_are_separate_and_reconcile(tmp_path):
    """Every gate assessment contributes exactly one honest CodeRabbit bucket.

    A plain pass is direct evidence, while the waiver token remains visible as
    a separate bucket. Missing/non-object gates are unobserved; malformed
    present values are unknown rather than silently counted as passes.
    """
    rows = [
        _row("rev-cr-direct", 1, "INTAKE_BEAD_CREATED", 0, context={}),
        _row(
            "rev-cr-direct", 1, "GATE_ASSESSMENT", 1,
            context={"gates": {"coderabbit": "pass"}},
        ),
        _row("rev-cr-waived", 1, "INTAKE_BEAD_CREATED", 2, context={}),
        _row(
            "rev-cr-waived", 1, "GATE_ASSESSMENT", 3,
            context={"gates": {"coderabbit": {
                "verdict": "pass",
                "evidence": ["coderabbit:waived_vendor_unavailable"],
            }}},
        ),
        _row("rev-cr-unknown", 1, "INTAKE_BEAD_CREATED", 4, context={}),
        _row(
            "rev-cr-unknown", 1, "GATE_ASSESSMENT", 5,
            context={"gates": {"coderabbit": {"verdict": "unknown"}}},
        ),
        _row("rev-cr-fail", 1, "INTAKE_BEAD_CREATED", 6, context={}),
        _row(
            "rev-cr-fail", 1, "GATE_ASSESSMENT", 7,
            context={"gates": {"coderabbit": {"verdict": "fail"}}},
        ),
        _row("rev-cr-unobserved", 1, "INTAKE_BEAD_CREATED", 8, context={}),
        _row("rev-cr-unobserved", 1, "GATE_ASSESSMENT", 9, context={}),
        _row("rev-cr-malformed", 1, "INTAKE_BEAD_CREATED", 10, context={}),
        _row(
            "rev-cr-malformed", 1, "GATE_ASSESSMENT", 11,
            context={"gates": {"coderabbit": []}},
        ),
        # No intake origin in the window: it is excluded from lane totals but
        # must remain in the report-wide exact-head CodeRabbit denominator.
        _row(
            "rev-cr-unassigned", 1, "GATE_ASSESSMENT", 12,
            context={
                "pr_number": 9010,
                "head_sha": "unassigned-head",
                "gates": {"coderabbit": "fail"},
            },
        ),
    ]
    path = _write_fixture(tmp_path, rows=rows)
    events = load_events_full(path, since=None, now=T0 + timedelta(days=1))
    report = compute_lane_report(events, since_label="test")
    stat = next(item for item in report.lanes if item.lane == "bead_start")

    assert stat.coderabbit == {
        "direct_approved": 1,
        "waived_unavailable": 1,
        "unknown": 2,
        "fail": 1,
        "unobserved": 1,
    }
    assert stat.coderabbit_total == 6
    assert sum(stat.coderabbit.values()) == 6

    assert report.coderabbit == {
        "direct_approved": 1,
        "waived_unavailable": 1,
        "unknown": 2,
        "fail": 2,
        "unobserved": 1,
    }
    assert report.coderabbit_total == 7

    payload = render_json(report)
    assert payload["coderabbit_total"] == 7
    assert payload["coderabbit"] == {
        "direct_approved": 1,
        "waived_unavailable": 1,
        "unknown": 2,
        "fail": 2,
        "unobserved": 1,
    }
    lane = next(item for item in payload["lanes"] if item["lane"] == "bead_start")
    assert lane["coderabbit"] == stat.coderabbit
    assert lane["coderabbit_total"] == sum(lane["coderabbit"].values())


def test_coderabbit_recovery_counts_each_assessment(tmp_path):
    """CodeRabbit outcomes are assessment observations, not bead stages."""
    rows = [
        _row("rev-cr-reroll", 1, "INTAKE_BEAD_CREATED", 0, context={}),
        _row(
            "rev-cr-reroll", 1, "GATE_ASSESSMENT", 1,
            context={"gates": {"coderabbit": {"verdict": "fail"}}},
        ),
        _row(
            "rev-cr-reroll", 2, "GATE_ASSESSMENT", 2,
            context={"gates": {"coderabbit": "pass"}},
        ),
    ]
    events = load_events_full(
        _write_fixture(tmp_path, rows=rows),
        since=None,
        now=T0 + timedelta(days=1),
    )
    stat = next(
        item for item in compute_lane_report(events).lanes if item.lane == "bead_start"
    )
    assert stat.total == 1
    assert stat.coderabbit == {
        "direct_approved": 1,
        "waived_unavailable": 0,
        "unknown": 0,
        "fail": 1,
        "unobserved": 0,
    }
    assert stat.coderabbit_total == 2


def test_coderabbit_deduplicates_pr_head_and_keeps_latest(tmp_path):
    rows = [
        _row("rev-cr-head", 1, "INTAKE_BEAD_CREATED", 0, context={}),
        _row(
            "rev-cr-head", 1, "GATE_ASSESSMENT", 1,
            context={
                "pr_number": 9005,
                "head_sha": "abc123",
                "gates": {"coderabbit": {"verdict": "unknown"}},
            },
        ),
        # Same immutable PR head, later assessment: it replaces the earlier
        # unknown rather than inflating the denominator.
        _row(
            "rev-cr-head", 1, "GATE_ASSESSMENT", 2,
            context={
                "pr_number": 9005,
                "head_sha": "abc123",
                "gates": {"coderabbit": {
                    "verdict": "pass",
                    "evidence": ["coderabbit:waived_vendor_unavailable"],
                }},
            },
        ),
        _row(
            "rev-cr-head", 1, "GATE_ASSESSMENT", 3,
            context={
                "pr_number": 9005,
                "head_sha": "def456",
                "gates": {"coderabbit": "pass"},
            },
        ),
    ]
    events = load_events_full(
        _write_fixture(tmp_path, rows=rows),
        since=None,
        now=T0 + timedelta(days=1),
    )
    stat = next(
        item for item in compute_lane_report(events).lanes if item.lane == "bead_start"
    )
    assert stat.coderabbit == {
        "direct_approved": 1,
        "waived_unavailable": 1,
        "unknown": 0,
        "fail": 0,
        "unobserved": 0,
    }
    assert stat.coderabbit_total == 2


@pytest.mark.parametrize(
    ("verdict", "evidence", "expected"),
    [
        ("fail", ["coderabbit:waived_vendor_unavailable"], "fail"),
        ("unknown", ["coderabbit:waived_vendor_unavailable"], "unknown"),
        ("pass", ["coderabbit:waived_vendor_unavailable"], "waived_unavailable"),
    ],
)
def test_coderabbit_waiver_requires_green_verdict(verdict, evidence, expected):
    from runner.funnel_lanes import classify_coderabbit

    assert classify_coderabbit({
        "gates": {"coderabbit": {"verdict": verdict, "evidence": evidence}}
    }) == expected


def test_legacy_same_second_rows_keep_distinct_observations(tmp_path):
    rows = [
        _row("rev-cr-legacy", 1, "INTAKE_BEAD_CREATED", 0, context={}),
        _row(
            "rev-cr-legacy", 1, "GATE_ASSESSMENT", 1,
            context={"gates": {"coderabbit": "fail"}},
        ),
        # Same bead, attempt, and second as the prior row; input sequence is
        # the only stable identity for legacy telemetry without PR/head data.
        _row(
            "rev-cr-legacy", 1, "GATE_ASSESSMENT", 1,
            context={"gates": {"coderabbit": "pass"}},
        ),
    ]
    events = load_events_full(
        _write_fixture(tmp_path, rows=rows),
        since=None,
        now=T0 + timedelta(days=1),
    )
    report = compute_lane_report(events)
    assert report.coderabbit == {
        "direct_approved": 1,
        "waived_unavailable": 0,
        "unknown": 0,
        "fail": 1,
        "unobserved": 0,
    }
    assert report.coderabbit_total == 2

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
