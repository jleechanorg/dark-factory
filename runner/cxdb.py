"""CXDB — Context DB. SQLite-backed event log of pipeline runs.

Every step the engine takes is recorded so the Healer can later cluster
failures by (node, outcome, output_hash) without re-reading the code.

Schema is intentionally narrow: one row per node visit. Output is truncated
to 4 KiB and hashed for clustering.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import re
import sqlite3
import time
import uuid
from typing import Iterable

SCHEMA = """
CREATE TABLE IF NOT EXISTS runs (
    run_id      TEXT PRIMARY KEY,
    pipeline    TEXT NOT NULL,
    goal        TEXT NOT NULL,
    started_ts  REAL NOT NULL,
    ended_ts    REAL,
    final       TEXT
);
CREATE TABLE IF NOT EXISTS steps (
    run_id        TEXT NOT NULL,
    seq           INTEGER NOT NULL,
    node          TEXT NOT NULL,
    outcome       TEXT NOT NULL,
    ts            REAL NOT NULL,
    output_hash   TEXT NOT NULL,
    output_head   TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    PRIMARY KEY (run_id, seq),
    FOREIGN KEY (run_id) REFERENCES runs(run_id)
);
CREATE INDEX IF NOT EXISTS idx_steps_node_outcome ON steps(node, outcome);
CREATE INDEX IF NOT EXISTS idx_steps_hash ON steps(output_hash);
"""


_MAX_OUTPUT = 4096

# Pytest (and many shell tools) emit a timing line in their output that
# varies run-to-run even for identical failures, e.g. "2 passed in 0.10s",
# "no tests ran in 0.13s", "FAILED in 1.4s". When we hash step output to
# cluster failures, those timing fragments produce phantom-distinct
# clusters for what is the same failure. Normalise them out before hashing.
#
# We strip the *value* of the time, not the surrounding text — so the
# *shape* of the line is preserved (still distinguishes "no tests ran"
# from "5 failed", etc.). Lines that don't match are passed through.
_PYTEST_TIMING_RE = re.compile(
    r"""
    \b in \s+ \d+(?:\.\d+)? s \b      # "in 0.10s"
    |
    \b \d+(?:\.\d+)? s \b              # bare "0.10s"
    """,
    re.VERBOSE,
)


def _normalise_for_hash(text: str) -> str:
    """Strip non-deterministic timing fragments before clustering."""
    if not text:
        return ""
    return _PYTEST_TIMING_RE.sub("<TIME>", text)


def _hash(text: str) -> str:
    return hashlib.sha256(
        _normalise_for_hash(text).encode("utf-8", errors="replace")
    ).hexdigest()[:16]


def _coerce_metric(value: object, cast: type) -> Optional[object]:
    """Return ``cast(value)`` or ``None`` on coercion failure.

    Matches the "skip unparseable, never raise" contract that
    :meth:`CXDB.cluster_aggregates` needs to survive rows whose
    ``metadata_json`` is partially malformed: some CXDB writers
    emit string numbers, others emit ``None``, others omit the
    key entirely. None and empty string are normalised to ``None``
    up front so a single downstream ``if n is not None`` check
    covers all three "absent" cases.
    """
    if value is None or value == "":
        return None
    try:
        return cast(value)
    except (TypeError, ValueError):
        return None


class CXDB:
    """Thin SQLite wrapper. Cheap to open per-run; not pooled."""

    def __init__(self, path: pathlib.Path):
        self.path = pathlib.Path(path).expanduser()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._conn = sqlite3.connect(str(self.path))
        # WAL + busy_timeout so concurrent pipelines into the same CXDB don't
        # collide on `database is locked`. README explicitly promises many
        # runs into one CXDB for the Healer.
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._conn.execute("PRAGMA busy_timeout=5000")
        self._conn.executescript(SCHEMA)
        self._conn.commit()

    def start_run(self, pipeline: str, goal: str) -> str:
        run_id = uuid.uuid4().hex[:12]
        self._conn.execute(
            "INSERT INTO runs(run_id, pipeline, goal, started_ts) VALUES(?, ?, ?, ?)",
            (run_id, pipeline, goal, time.time()),
        )
        self._conn.commit()
        return run_id

    def record_step(
        self,
        run_id: str,
        seq: int,
        node: str,
        outcome: str,
        ts: float,
        output: str,
        metadata: dict,
    ) -> None:
        head = (output or "")[:_MAX_OUTPUT]
        self._conn.execute(
            """
            INSERT INTO steps(run_id, seq, node, outcome, ts, output_hash, output_head, metadata_json)
            VALUES(?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                run_id,
                seq,
                node,
                outcome,
                ts,
                _hash(head),
                head,
                json.dumps(metadata or {}, ensure_ascii=False),
            ),
        )
        self._conn.commit()

    def end_run(self, run_id: str, final: str) -> None:
        self._conn.execute(
            "UPDATE runs SET ended_ts = ?, final = ? WHERE run_id = ?",
            (time.time(), final, run_id),
        )
        self._conn.commit()

    def cluster_aggregates(
        self, node: str, outcome: str, output_hash: str
    ) -> dict:
        """Sum tokens / cost / wall_ms across every row in a failure cluster.

        Used by the Healer to surface aggregate cost columns per cluster.
        Each row's `metadata_json` is decoded and the numeric metric keys are
        summed. Missing or unparseable values are simply skipped — they do not
        contribute and they do not raise.
        """
        cur = self._conn.cursor()
        cur.execute(
            """
            SELECT metadata_json FROM steps
            WHERE node = ? AND outcome = ? AND output_hash = ?
            """,
            (node, outcome, output_hash),
        )
        total_tokens = 0
        total_cost_usd = 0.0
        total_wall_ms = 0
        saw_any = False
        for (raw,) in cur.fetchall():
            try:
                meta = json.loads(raw or "{}")
            except (TypeError, ValueError):
                continue
            for k in ("tokens_in", "tokens_out", "tokens_total"):
                n = _coerce_metric(meta.get(k), int)
                if n is not None:
                    total_tokens += n
                    saw_any = True
            cost = _coerce_metric(meta.get("cost_usd"), float)
            if cost is not None:
                total_cost_usd += cost
                saw_any = True
            wall = _coerce_metric(meta.get("wall_ms"), int)
            if wall is not None:
                total_wall_ms += wall
                saw_any = True
        return {
            "total_tokens": total_tokens if saw_any else None,
            "total_cost_usd": total_cost_usd if saw_any else None,
            "total_wall_ms": total_wall_ms if saw_any else None,
        }

    def failed_steps(self) -> Iterable[sqlite3.Row]:
        cur = self._conn.cursor()
        cur.row_factory = sqlite3.Row
        cur.execute(
            """
            SELECT node, outcome, output_hash, COUNT(*) AS hits,
                   GROUP_CONCAT(run_id, ',') AS run_ids,
                   MAX(output_head) AS sample
            FROM steps
            WHERE outcome IN (
                'failure', 'fail', 'error', 'exhausted', 'stuck', 'partial', 'inconclusive'
            )
            GROUP BY node, outcome, output_hash
            ORDER BY hits DESC, node ASC
            """,
        )
        return list(cur.fetchall())

    def close(self) -> None:
        self._conn.close()
