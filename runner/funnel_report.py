"""Funnel report — incoming task -> /ready-PR throughput metric (bead rev-2vqpa).

ROOT CAUSE this fixes: dark-factory has two disconnected telemetry stores —
``runner/cxdb.py`` (scoped to a single pipeline run) and the daemon's
``daemon.jsonl`` (scoped to bead-lifecycle events, see
``daemon/src/telemetry.rs``) — but nothing aggregates either into a
queryable throughput metric. This module reads ``daemon.jsonl`` only (CXDB
already has its own report path: ``runner/healer.py``).

Stage chain (the "happy path" a bead travels along):

    TASK_DISPATCHED -> PR_OPENED -> GATE_ASSESSMENT -> READY_FOR_MERGE

``PARKED_HUMAN_HELD`` and ``ESCALATION_REQUIRED`` are reported as a separate
"diverted" section rather than appended to the hop-to-hop chain: they are
terminal states that can fire at *any* point in the chain (a bead can park
before ever opening a PR), so a naive "hop-to-hop latency from
READY_FOR_MERGE to PARKED_HUMAN_HELD" would average together unrelated
divert points and produce a meaningless number. Reporting them as counts +
percent-of-dispatched keeps the number honest.

Event-type strings are grepped verbatim from ``daemon/src/tick.rs``
(2026-08-23): ``TASK_DISPATCHED``, ``PR_OPENED``, ``GATE_ASSESSMENT``,
``READY_FOR_MERGE``, ``PARKED_HUMAN_HELD``, ``ESCALATION_REQUIRED``.

Field names are read defensively because ``daemon.jsonl`` is an
append-only rolling log spanning multiple schema revisions in production
(confirmed against the live ``~/Library/Logs/dark-factory/daemon.jsonl``):
the current ``daemon/src/telemetry.rs`` emits
``{timestamp, beadId, attemptId, lifecycleState, eventType, metrics,
context}``, but older rows use ``{ts, beadId, attempt, state, eventType,
counts, context}``. Both are accepted transparently in ``_normalize``.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Optional

# Ordered "happy path" a dispatched bead travels along toward a ready PR.
MAIN_STAGES = ["TASK_DISPATCHED", "PR_OPENED", "GATE_ASSESSMENT", "READY_FOR_MERGE"]

# Terminal diversions off the happy path — reported separately (see module
# docstring for why they are not chained onto MAIN_STAGES).
SIDE_STAGES = ["PARKED_HUMAN_HELD", "ESCALATION_REQUIRED"]


def default_daemon_log_path() -> pathlib.Path:
    """Default daemon.jsonl location, mirroring the ``--perf-log-dir``
    convention documented in CLAUDE.md / ``runner/__main__.py``
    (``~/Library/Logs/dark-factory``) — daemon.jsonl lives directly under
    that root. Never defaults to ``/tmp`` (that convention exists precisely
    because /tmp periodic sweeps lose data — see CLAUDE.md)."""
    return pathlib.Path.home() / "Library" / "Logs" / "dark-factory" / "daemon.jsonl"


_SINCE_RE = re.compile(r"^(\d+)\s*([smhd])$", re.IGNORECASE)
_SINCE_UNITS = {"s": "seconds", "m": "minutes", "h": "hours", "d": "days"}


def parse_since(value: str) -> timedelta:
    """Parse a ``--since`` duration like ``'48h'``, ``'30m'``, ``'7d'``, ``'90s'``."""
    m = _SINCE_RE.match(value.strip())
    if not m:
        raise ValueError(f"invalid --since value: {value!r} (expected e.g. '48h', '7d', '30m')")
    n = int(m.group(1))
    unit = _SINCE_UNITS[m.group(2).lower()]
    return timedelta(**{unit: n})


def _parse_ts(raw: Optional[str]) -> Optional[datetime]:
    if not raw:
        return None
    try:
        s = raw[:-1] + "+00:00" if raw.endswith("Z") else raw
        dt = datetime.fromisoformat(s)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.astimezone(timezone.utc)
    except (ValueError, TypeError):
        return None


def _normalize(raw: dict) -> Optional[dict]:
    """Normalize one daemon.jsonl row across schema revisions. Returns
    ``None`` for rows missing a field the funnel needs (event type / bead
    id / timestamp) rather than raising — one malformed or pre-schema row
    must not sink the whole report."""
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
    return {"event_type": event_type, "bead_id": bead_id, "attempt_id": attempt_id, "ts": ts}


def load_events(
    path: pathlib.Path,
    since: Optional[timedelta] = None,
    now: Optional[datetime] = None,
) -> list[dict]:
    """Stream-read + normalize a daemon.jsonl file, dropping rows outside
    the ``since`` window (when given). Malformed JSON lines are skipped."""
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
            evt = _normalize(raw)
            if evt is None:
                continue
            if cutoff is not None and evt["ts"] < cutoff:
                continue
            events.append(evt)
    return events


@dataclass
class StageStat:
    stage: str
    count: int
    conversion_pct: Optional[float]  # None for the first stage in the chain
    latency_p50_s: Optional[float]
    latency_p95_s: Optional[float]


@dataclass
class SideStat:
    stage: str
    count: int
    pct_of_dispatched: Optional[float]


@dataclass
class FunnelReport:
    since_label: str
    total_lifecycles: int
    main_stages: list  # list[StageStat]
    side_stages: list  # list[SideStat]


def _percentile(values: list[float], pct: float) -> Optional[float]:
    """Linear-interpolation percentile (same convention as numpy's default
    'linear' method) over a list of latency samples in seconds."""
    if not values:
        return None
    s = sorted(values)
    if len(s) == 1:
        return s[0]
    k = (len(s) - 1) * pct
    f = int(k)
    c = min(f + 1, len(s) - 1)
    if f == c:
        return s[f]
    d = k - f
    return s[f] + (s[c] - s[f]) * d


def compute_funnel(events: list[dict], since_label: str = "") -> FunnelReport:
    """Group events by ``(bead_id, attempt_id)``; take the min timestamp
    per stage event type per lifecycle; compute hop-to-hop counts,
    conversion %, and p50/p95 latency along ``MAIN_STAGES``, plus
    ``SIDE_STAGES`` diversion counts (percent of dispatched)."""
    lifecycles: dict[tuple, dict[str, datetime]] = {}
    for evt in events:
        et = evt["event_type"]
        if et not in MAIN_STAGES and et not in SIDE_STAGES:
            continue
        key = (evt["bead_id"], evt["attempt_id"])
        stage_map = lifecycles.setdefault(key, {})
        prev = stage_map.get(et)
        if prev is None or evt["ts"] < prev:
            stage_map[et] = evt["ts"]

    total_lifecycles = len(lifecycles)

    main_stats: list[StageStat] = []
    prev_stage: Optional[str] = None
    prev_count: Optional[int] = None
    for stage in MAIN_STAGES:
        count = sum(1 for sm in lifecycles.values() if stage in sm)
        conversion: Optional[float] = None
        latencies: list[float] = []
        if prev_stage is not None:
            if prev_count:
                conversion = 100.0 * count / prev_count
            for sm in lifecycles.values():
                if prev_stage in sm and stage in sm:
                    delta = (sm[stage] - sm[prev_stage]).total_seconds()
                    if delta >= 0:
                        latencies.append(delta)
        main_stats.append(
            StageStat(
                stage=stage,
                count=count,
                conversion_pct=conversion,
                latency_p50_s=_percentile(latencies, 0.50),
                latency_p95_s=_percentile(latencies, 0.95),
            )
        )
        prev_stage = stage
        prev_count = count

    dispatched_count = main_stats[0].count if main_stats else 0
    side_stats: list[SideStat] = []
    for stage in SIDE_STAGES:
        count = sum(1 for sm in lifecycles.values() if stage in sm)
        pct = (100.0 * count / dispatched_count) if dispatched_count else None
        side_stats.append(SideStat(stage=stage, count=count, pct_of_dispatched=pct))

    return FunnelReport(
        since_label=since_label,
        total_lifecycles=total_lifecycles,
        main_stages=main_stats,
        side_stages=side_stats,
    )


def _fmt_pct(v: Optional[float]) -> str:
    return "—" if v is None else f"{v:.1f}%"


def _fmt_secs(v: Optional[float]) -> str:
    return "—" if v is None else f"{v:.0f}s"


def render_markdown(report: FunnelReport) -> str:
    lines = [
        f"# df-funnel report (since {report.since_label or 'start of log'})",
        "",
        f"Lifecycles observed (bead, attempt) in window: {report.total_lifecycles}",
        "",
        "## Main funnel",
        "",
        "| Stage | Count | Conversion from prev | Latency p50 | Latency p95 |",
        "|---|---|---|---|---|",
    ]
    for s in report.main_stages:
        lines.append(
            f"| {s.stage} | {s.count} | {_fmt_pct(s.conversion_pct)} | "
            f"{_fmt_secs(s.latency_p50_s)} | {_fmt_secs(s.latency_p95_s)} |"
        )
    lines += [
        "",
        "## Diverted (terminal, not part of the happy path)",
        "",
        "| Stage | Count | % of dispatched |",
        "|---|---|---|",
    ]
    for s in report.side_stages:
        lines.append(f"| {s.stage} | {s.count} | {_fmt_pct(s.pct_of_dispatched)} |")
    lines.append("")
    return "\n".join(lines)


def render_json(report: FunnelReport) -> dict:
    return {
        "since": report.since_label,
        "total_lifecycles": report.total_lifecycles,
        "main_stages": [
            {
                "stage": s.stage,
                "count": s.count,
                "conversion_pct": s.conversion_pct,
                "latency_p50_s": s.latency_p50_s,
                "latency_p95_s": s.latency_p95_s,
            }
            for s in report.main_stages
        ],
        "side_stages": [
            {"stage": s.stage, "count": s.count, "pct_of_dispatched": s.pct_of_dispatched}
            for s in report.side_stages
        ],
    }


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="dark-factory-funnel")
    p.add_argument(
        "--daemon-log",
        type=pathlib.Path,
        default=None,
        help="Path to daemon.jsonl (default: ~/Library/Logs/dark-factory/daemon.jsonl)",
    )
    p.add_argument(
        "--since",
        default="48h",
        help="Time window, e.g. '48h', '7d', '30m' (default: 48h)",
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

    events = load_events(log_path, since=window)
    report = compute_funnel(events, since_label=args.since)

    if args.json:
        print(json.dumps(render_json(report), indent=2))
    else:
        print(render_markdown(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
