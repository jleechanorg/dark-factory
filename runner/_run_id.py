"""Unique run-id generation for Dark Factory invocations.

Goal
----
Every ``bin/dark-factory`` invocation needs a unique identifier so the
runner, the CXDB event log, the perf-log directory, and the
``~/.dark-factory/merge_train/`` lock files can all reference the same
run by id. The previous version of ``bin/dark-factory`` generated the
id inline (``df-<ns_timestamp>-<pid>``) which made the format hard to
test, hard to evolve, and inconsistent with the
``ctx.state["run_id"]`` the runner already assigns.

This module is the single source of truth for run-id generation.
``bin/dark-factory`` and any future programmatic invocation point at
this helper so the format is uniform across the wrapper, the runner,
and the lock files.

API
---
- :func:`generate` — return a fresh run id.
- :func:`is_valid` — cheap syntactic check for whether a string is a
  run id in the expected format.

Format
------
``df-<unix-ns>-<pid>`` where:

- ``df`` is the literal prefix (Dark Factory).
- ``<unix-ns>`` is the current time in nanoseconds since the Unix
  epoch, zero-padded to 19 digits.
- ``<pid>`` is the current process id (no padding).

Example: ``df-1781333039990123456-37132``

The format is greppable, sortable, and unique to the nanosecond on a
single host. Two processes started in the same nanosecond from
different pids do not collide. Two processes started from the same pid
across a 1+ second boundary do not collide. The kernel does not reuse
pids inside a single boot, so the tuple is unique per boot per host.
"""

from __future__ import annotations

import os
import re
import time
from typing import Optional

_PREFIX = "df"
_FORMAT_RE = re.compile(r"^df-\d{19}-\d+$")


def generate(*, now_ns: Optional[int] = None) -> str:
    """Return a fresh, unique run id.

    Parameters
    ----------
    now_ns:
        Override the current time in nanoseconds since the Unix epoch.
        Tests pass a controlled value to keep the assertion stable.
        Defaults to ``time.time_ns()``.

    Returns
    -------
    A run id of the form ``df-<unix-ns>-<pid>``.
    """
    ns = now_ns if now_ns is not None else time.time_ns()
    return f"{_PREFIX}-{ns:019d}-{os.getpid()}"


def is_valid(run_id: str) -> bool:
    """Return True iff ``run_id`` is a syntactically valid run id.

    This is a fast format check; it does not verify uniqueness. Used
    by the runner when reading a run id from an external artifact
    (CXDB replay, log reader) to fail loud on corruption rather than
    silently misattribute events to the wrong run.
    """
    if not isinstance(run_id, str) or not run_id:
        return False
    return bool(_FORMAT_RE.match(run_id))


def parse(run_id: str) -> Optional[tuple[int, int]]:
    """Return ``(unix_ns, pid)`` for a valid run id, else ``None``.

    Companion to :func:`is_valid`; useful for log analysis and tests
    that want to assert on the embedded timestamp or pid.
    """
    if not is_valid(run_id):
        return None
    _, ns_str, pid_str = run_id.split("-", 2)
    return int(ns_str), int(pid_str)
