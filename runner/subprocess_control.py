"""Bounded subprocess execution with process-group cleanup."""

from __future__ import annotations

import os
import signal
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Sequence


def as_text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


@dataclass(frozen=True, slots=True)
class BoundedProcessResult:
    args: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str
    timed_out: bool


def _signal_process_group(proc: subprocess.Popen, sig: signal.Signals) -> None:
    try:
        os.killpg(proc.pid, sig)
    except ProcessLookupError:
        pass


def finish_bounded_process(
    proc: subprocess.Popen,
    *,
    timeout: float,
    input_text: str | None = None,
    terminate_grace: float = 5.0,
) -> BoundedProcessResult:
    """Communicate within ``timeout`` and always reap the whole process group."""
    timed_out = False
    try:
        stdout, stderr = proc.communicate(input=input_text, timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        _signal_process_group(proc, signal.SIGTERM)
        try:
            stdout, stderr = proc.communicate(timeout=terminate_grace)
        except subprocess.TimeoutExpired:
            _signal_process_group(proc, signal.SIGKILL)
            stdout, stderr = proc.communicate()
    return BoundedProcessResult(
        args=tuple(str(part) for part in proc.args),
        returncode=int(proc.returncode if proc.returncode is not None else -1),
        stdout=as_text(stdout),
        stderr=as_text(stderr),
        timed_out=timed_out,
    )


def run_bounded_process(
    args: Sequence[str],
    *,
    cwd: str | Path | None = None,
    input_text: str | None = None,
    timeout: float,
    env: Mapping[str, str] | None = None,
    terminate_grace: float = 5.0,
) -> BoundedProcessResult:
    proc = subprocess.Popen(
        list(args),
        cwd=cwd,
        stdin=subprocess.PIPE if input_text is not None else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
        env=dict(env) if env is not None else None,
    )
    return finish_bounded_process(
        proc,
        timeout=timeout,
        input_text=input_text,
        terminate_grace=terminate_grace,
    )
