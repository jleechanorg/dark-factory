"""Factory funnel, split by intake origin lane (bead rev-2vqpa follow-up).

ROOT CAUSE this fixes: ``runner/funnel_report.py`` aggregates ALL lifecycles
into one funnel regardless of how they entered the factory. That hides a
real distinction operators care about — 2026-08-24 live analysis found the
three entry points behave very differently:

  - **bead_start**: a bead hand-created with no external reference
    (``INTAKE_BEAD_CREATED`` with no ``context.external_ref``).
  - **gh_issue_start**: a bead created by sweeping a GitHub issue
    (``INTAKE_BEAD_CREATED`` with ``context.external_ref`` set).
  - **pr_adopted_start**: a bead created by adopting an already-open PR
    (``EXISTING_PR_ADOPTED`` with ``context.newly_created`` true — the PR
    predates the bead, so ``PR_OPENED`` never fires for this lane; gate
    assessment starts immediately instead).

For each lane, this module reports the **furthest main-funnel stage
reached** per lifecycle (not just aggregate counts) — "how far did each
lane get" — plus the terminal divert breakdown (``PARKED_HUMAN_HELD``,
``ESCALATION_REQUIRED``, or still active).

Reuses ``runner.funnel_report``'s schema-tolerant timestamp parsing and
stage vocabulary rather than duplicating it — see that module's docstring
for the full schema-drift rationale (current vs legacy daemon.jsonl rows).

Known caveat (documented 2026-08-24 live run, do not silently "fix" without
re-verifying): ``TASK_DISPATCHED`` is not emitted on every dispatch code
path (e.g. bead ``jleechan-wjm2`` was confirmed dispatched-and-merged via
its own bead notes — "daemon routed SMALL_PATH -> AO spawn -> merged" — but
shows no ``TASK_DISPATCHED`` event in daemon.jsonl). The "never dispatched"
count in the ``INTAKE_ONLY`` bucket is therefore a directional signal, not
an exact count; cross-check against bead notes before treating any single
bead as proof of starvation.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Optional

from runner.funnel_report import (
    MAIN_STAGES,
    SIDE_STAGES,
    _parse_ts,
    default_daemon_log_path,
    parse_since,
)

LANES = ["bead_start", "gh_issue_start", "pr_adopted_start"]

_STAGE_RANK = {s: i for i, s in enumerate(MAIN_STAGES)}
_SIDE_SET = set(SIDE_STAGES)
_MAIN_SET = set(MAIN_STAGES)


def _normalize_full(raw: dict) -> Optional[dict]:
    """Like ``funnel_report._normalize`` but keeps ``context`` — lane
    classification needs ``context.external_ref`` / ``context.newly_created``,
    which the base module intentionally drops (it doesn't need them)."""
    event_type = raw.get("eventType")
    bead_id = raw.get("beadId")
    ts = _parse_ts(raw.get("timestamp") or raw.get("ts"))
    if not event_type or not bead_id or ts is None:
        return None
    attempt = raw.get("attemptId")
    if attempt is None:
        attempt = raw.get("attempt")
    try:
        attempt_id = int(attempt) if attempt is not None else 0
    except (TypeError, ValueError):
        attempt_id = 0
    context = raw.get("context") or {}
    if not isinstance(context, dict):
        context = {}
    return {
        "event_type": event_type,
        "bead_id": bead_id,
        "attempt_id": attempt_id,
        "ts": ts,
        "context": context,
    }


def load_events_full(path: pathlib.Path, since=None, now=None) -> list[dict]:
    """Stream-read + normalize daemon.jsonl, keeping ``context`` for lane
    classification. Malformed rows are skipped, never fatal — see
    ``funnel_report.load_events`` for the same contract."""
    if now is None:
        now = datetime.now(timezone.utc)
    cutoff = (now - since) if since is not None else None
    events: list[dict] = []
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                raw = json.loads(line)
            except json.JSONDecodeError:
                continue
            evt = _normalize_full(raw)
            if evt is None:
                continue
            if cutoff is not None and evt["ts"] < cutoff:
                continue
            events.append(evt)
    return events


@dataclass
class LaneStat:
    lane: str
    total: int
    furthest: dict = field(default_factory=dict)  # {"INTAKE_ONLY"|stage: count}
    terminal: dict = field(default_factory=dict)  # {"none"|side_stage: count}


@dataclass
class LaneReport:
    since_label: str
    lanes: list  # list[LaneStat]


def classify_origin(events: list[dict]) -> dict[str, str]:
    """First INTAKE_BEAD_CREATED / EXISTING_PR_ADOPTED event per **bead_id**
    (NOT per (bead_id, attempt_id)) determines that bead's lane. Origin is a
    property of the bead itself, not of an individual reroll attempt.

    BUG FIXED 2026-08-24 (found live, before this was true the numbers were
    silently wrong): keying by (bead_id, attempt_id) here caused every
    lifecycle whose origin event fired on one attempt but whose downstream
    stage events (GATE_ASSESSMENT, READY_FOR_MERGE, ...) fired on a LATER
    reroll attempt to be silently excluded from every lane — the join key
    never matched. Confirmed on real data: bead ``dark-factory-4sey`` had
    ``EXISTING_PR_ADOPTED`` only at attempts 1-2, but its ``READY_FOR_MERGE``
    fired at attempt 3; bead ``jleechan-l3r6`` had ``INTAKE_BEAD_CREATED``
    only at attempt 1, but ``READY_FOR_MERGE`` fired at attempts 2 AND 4.
    Both were invisibly dropped from the 2026-08-24 initial 30d report,
    which claimed 0 READY_FOR_MERGE across all lanes when the correct
    number (keying by bead_id) is non-zero. Events are assumed roughly
    time-ordered as read from the log (daemon.jsonl is append-only); the
    first classifying event wins."""
    origin: dict[str, str] = {}
    for evt in events:
        bead_id = evt["bead_id"]
        if bead_id in origin:
            continue
        et = evt["event_type"]
        ctx = evt["context"]
        if et == "INTAKE_BEAD_CREATED":
            origin[bead_id] = "gh_issue_start" if ctx.get("external_ref") else "bead_start"
        elif et == "EXISTING_PR_ADOPTED" and ctx.get("newly_created"):
            origin[bead_id] = "pr_adopted_start"
    return origin


def compute_lane_report(events: list[dict], since_label: str = "") -> LaneReport:
    """Reports are aggregated at the **bead level** — across ALL reroll
    attempts of a bead, not per-(bead_id, attempt_id) lifecycle. A bead's
    lane origin is fixed once (first classifying event); "furthest
    milestone reached" is the max stage that bead EVER reached on any
    attempt; "terminal divert" reflects the bead's LATEST (highest
    attempt_id) attempt only, so a bead that parked on attempt 2 but was
    successfully rerolled and reached READY_FOR_MERGE on attempt 3 is NOT
    misreported as still-parked."""
    origin = classify_origin(events)  # bead_id -> lane

    bead_events: dict[str, list[tuple[int, str]]] = {}
    for evt in events:
        et = evt["event_type"]
        if et not in _MAIN_SET and et not in _SIDE_SET:
            continue
        bead_id = evt["bead_id"]
        if bead_id not in origin:
            continue
        bead_events.setdefault(bead_id, []).append((evt["attempt_id"], et))

    lane_stats: dict[str, LaneStat] = {lane: LaneStat(lane=lane, total=0) for lane in LANES}

    for bead_id, lane in origin.items():
        stat = lane_stats[lane]
        stat.total += 1
        evs = bead_events.get(bead_id, [])

        main_hit = [et for (_a, et) in evs if et in _MAIN_SET]
        furthest = max(main_hit, key=lambda x: _STAGE_RANK[x]) if main_hit else "INTAKE_ONLY"
        stat.furthest[furthest] = stat.furthest.get(furthest, 0) + 1

        if evs:
            latest_attempt = max(a for a, _et in evs)
            latest_side = [et for (a, et) in evs if a == latest_attempt and et in _SIDE_SET]
            terminal = latest_side[-1] if latest_side else "none"
        else:
            terminal = "none"
        stat.terminal[terminal] = stat.terminal.get(terminal, 0) + 1

    return LaneReport(since_label=since_label, lanes=[lane_stats[lane] for lane in LANES])


def _fmt_pct(n: int, total: int) -> str:
    if total == 0:
        return "—"
    return f"{100.0 * n / total:.0f}%"


def render_markdown(report: LaneReport) -> str:
    lines = [
        f"# df-funnel-lanes report (since {report.since_label or 'start of log'})",
        "",
        "Lanes: bead_start (hand-created bead) | gh_issue_start (swept GH issue) | "
        "pr_adopted_start (adopted an already-open PR)",
        "",
    ]
    stage_order = ["INTAKE_ONLY"] + MAIN_STAGES
    for stat in report.lanes:
        lines.append(f"## {stat.lane} (n={stat.total})")
        lines.append("")
        lines.append("### Furthest milestone reached")
        lines.append("")
        lines.append("| Milestone | Count | % |")
        lines.append("|---|---|---|")
        for s in stage_order:
            n = stat.furthest.get(s, 0)
            lines.append(f"| {s} | {n} | {_fmt_pct(n, stat.total)} |")
        lines.append("")
        lines.append("### Terminal divert")
        lines.append("")
        lines.append("| Divert | Count | % |")
        lines.append("|---|---|---|")
        for s, n in sorted(stat.terminal.items(), key=lambda kv: -kv[1]):
            lines.append(f"| {s} | {n} | {_fmt_pct(n, stat.total)} |")
        lines.append("")
    return "\n".join(lines)


def render_json(report: LaneReport) -> dict:
    return {
        "since": report.since_label,
        "lanes": [
            {
                "lane": stat.lane,
                "total": stat.total,
                "furthest_stage": dict(stat.furthest),
                "terminal_divert": dict(stat.terminal),
            }
            for stat in report.lanes
        ],
    }


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="dark-factory-funnel-lanes")
    p.add_argument(
        "--daemon-log",
        type=pathlib.Path,
        default=None,
        help="Path to daemon.jsonl (default: ~/Library/Logs/dark-factory/daemon.jsonl)",
    )
    p.add_argument(
        "--since",
        default="30d",
        help="Time window, e.g. '30d', '7d', '48h' (default: 30d — intake events are "
        "sparse; a short window under-samples the bead_start/gh_issue_start lanes)",
    )
    p.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-parseable JSON instead of a Markdown table",
    )
    args = p.parse_args(argv)

    log_path = args.daemon_log or default_daemon_log_path()
    if not log_path.exists():
        print(f"daemon log not found: {log_path}", file=sys.stderr)
        return 1

    try:
        window = parse_since(args.since)
    except ValueError as e:
        print(str(e), file=sys.stderr)
        return 2

    events = load_events_full(log_path, since=window)
    report = compute_lane_report(events, since_label=args.since)

    if args.json:
        print(json.dumps(render_json(report), indent=2))
    else:
        print(render_markdown(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
