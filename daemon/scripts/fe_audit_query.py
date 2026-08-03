#!/usr/bin/env python3
"""fe_audit_query.py — tolerant JSONL telemetry parser for fe-audit.sh.

Reads daemon.jsonl line by line, skips malformed lines (silent), and emits
results for one of four queries:
  - g10_ticks    : last N TICK event timestamps
  - g11_attested : bead IDs with lifecycleState=ATTESTED in lookback
  - g11_dispatched : bead IDs with lifecycleState=DISPATCHED in lookback
  - g12_transient : bead IDs whose transient-error event count >= threshold
  - g13_dispatch_rate : hour-buckets whose dispatch count > cap

Usage:
    fe_audit_query.py <query> <log_file> <cutoff_iso> [threshold]

Exit codes:
    0   success
    2   invalid args
    3   unknown query
"""
import json
import sys
from collections import Counter, defaultdict


def parse_log(path):
    """Yield each successfully-parsed record; skip malformed lines silently."""
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def g10_ticks(records, cutoff):
    """Emit the last few TICK timestamps after cutoff (one per line)."""
    seen = set()
    for rec in records:
        if rec.get("eventType") == "TICK" and rec.get("timestamp", "") >= cutoff:
            ts = rec.get("timestamp", "")
            if ts and ts not in seen:
                seen.add(ts)
                yield ts


def g11_attested(records, cutoff):
    """Unique bead IDs with lifecycleState=ATTESTED after cutoff."""
    for rec in records:
        if (
            rec.get("lifecycleState") == "ATTESTED"
            and rec.get("timestamp", "") >= cutoff
        ):
            bid = rec.get("beadId", "")
            if bid:
                yield bid


def g11_dispatched(records, cutoff):
    """Unique bead IDs with lifecycleState=DISPATCHED after cutoff."""
    for rec in records:
        if (
            rec.get("lifecycleState") == "DISPATCHED"
            and rec.get("timestamp", "") >= cutoff
        ):
            bid = rec.get("beadId", "")
            if bid:
                yield bid


def g11_human_held(records, cutoff):
    """Unique bead IDs with lifecycleState=HUMAN_HELD after cutoff.

    A bead that has legitimately escalated to HUMAN_HELD (branch-conflict
    recovery limit, external-dep blocker, etc.) is NOT stuck — it's parked
    for operator action per the dispatch-health triage flow. The G11 audit
    subtracts this set from ATTESTED so legitimate holds do not generate
    phantom factory-labeled beads that /af would re-dispatch against
    already-handled work (cf. phantom-dispatch cluster 74wt/lwte/z284/
    bze8.4 from 2026-08-01).
    """
    for rec in records:
        if (
            rec.get("lifecycleState") == "HUMAN_HELD"
            and rec.get("timestamp", "") >= cutoff
        ):
            bid = rec.get("beadId", "")
            if bid:
                yield bid


def g12_transient(records, cutoff, threshold):
    """Lines: '<count> <beadId>' for beads with >= threshold transient errors."""
    counter = Counter()
    for rec in records:
        et = rec.get("eventType", "")
        if "TRANSIENT_ERROR" in et and rec.get("timestamp", "") >= cutoff:
            bid = rec.get("beadId", "")
            if bid:
                counter[bid] += 1
    for bid, count in counter.most_common():
        if count >= threshold:
            yield f"{count} {bid}"


def g13_dispatch_rate(records, cutoff, cap):
    """Lines: '<YYYY-MM-DDTHH>: <count>' for hours with count > cap."""
    per_hour = Counter()
    for rec in records:
        if (
            rec.get("eventType") == "TASK_DISPATCHED"
            and rec.get("timestamp", "") >= cutoff
        ):
            ts = rec.get("timestamp", "")
            if len(ts) >= 13:
                per_hour[ts[:13]] += 1
    for hour, count in sorted(per_hour.items()):
        if count > cap:
            yield f"{hour}: {count} dispatches"


def main():
    if len(sys.argv) < 4:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    query = sys.argv[1]
    log_file = sys.argv[2]
    cutoff = sys.argv[3]
    threshold = int(sys.argv[4]) if len(sys.argv) > 4 else 0

    try:
        records = list(parse_log(log_file))
    except OSError as exc:
        print(f"fe_audit_query: cannot read {log_file}: {exc}", file=sys.stderr)
        sys.exit(9)

    if query == "g10_ticks":
        # Print only the last 3 unique timestamps, sorted ascending.
        ticks = sorted(set(g10_ticks(records, cutoff)))
        for ts in ticks[-3:]:
            print(ts)
    elif query == "g11_attested":
        for bid in sorted(set(g11_attested(records, cutoff))):
            print(bid)
    elif query == "g11_dispatched":
        for bid in sorted(set(g11_dispatched(records, cutoff))):
            print(bid)
    elif query == "g11_human_held":
        for bid in sorted(set(g11_human_held(records, cutoff))):
            print(bid)
    elif query == "g12_transient":
        for line in g12_transient(records, cutoff, threshold):
            print(line)
    elif query == "g13_dispatch_rate":
        for line in g13_dispatch_rate(records, cutoff, threshold):
            print(line)
    else:
        print(f"fe_audit_query: unknown query: {query}", file=sys.stderr)
        sys.exit(3)


if __name__ == "__main__":
    main()