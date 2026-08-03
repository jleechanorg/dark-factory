"""Bounded subprocess execution with process-group cleanup."""

from __future__ import annotations

import os
import signal
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Sequence


def as_text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


def _merge_output(current: str | bytes | None, later: str | bytes | None) -> str:
    current_text = as_text(current)
    later_text = as_text(later)
    if not later_text or later_text == current_text:
        return current_text
    if later_text.startswith(current_text):
        return later_text
    if current_text.endswith(later_text):
        return current_text
    return current_text + later_text


@dataclass(frozen=True, slots=True)
class BoundedProcessResult:
    args: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str
    timed_out: bool


def _signal_process_group(pgid: int, sig: signal.Signals) -> None:
    try:
        os.killpg(pgid, sig)
    except OSError:
        pass


def _process_group_exists(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def finish_bounded_process(
    proc: subprocess.Popen,
    *,
    timeout: float,
    input_text: str | None = None,
    terminate_grace: float = 5.0,
    process_group_id: int | None = None,
) -> BoundedProcessResult:
    """Communicate within ``timeout`` and always reap the whole process group."""
    timed_out = False
    pgid = int(process_group_id if process_group_id is not None else proc.pid)
    try:
        if input_text is None:
            stdout, stderr = proc.communicate(timeout=timeout)
        else:
            stdout, stderr = proc.communicate(input=input_text, timeout=timeout)
    except subprocess.TimeoutExpired as initial_timeout:
        timed_out = True
        stdout, stderr = as_text(initial_timeout.stdout), as_text(initial_timeout.stderr)
        _signal_process_group(pgid, signal.SIGTERM)
        deadline = time.monotonic() + max(0.0, terminate_grace)
        try:
            drained_stdout, drained_stderr = proc.communicate(timeout=terminate_grace)
            stdout, stderr = drained_stdout, drained_stderr
        except subprocess.TimeoutExpired as cleanup_timeout:
            stdout = _merge_output(stdout, cleanup_timeout.stdout)
            stderr = _merge_output(stderr, cleanup_timeout.stderr)
        except Exception:
            pass
        while _process_group_exists(pgid) and time.monotonic() < deadline:
            time.sleep(0.02)
        if _process_group_exists(pgid):
            _signal_process_group(pgid, signal.SIGKILL)
        try:
            final_stdout, final_stderr = proc.communicate(timeout=max(0.1, terminate_grace))
            if final_stdout is not None:
                stdout = final_stdout
            if final_stderr is not None:
                stderr = final_stderr
        except subprocess.TimeoutExpired as final_timeout:
            stdout = _merge_output(stdout, final_timeout.stdout)
            stderr = _merge_output(stderr, final_timeout.stderr)
        except Exception:
            pass
    return BoundedProcessResult(
        args=tuple(str(part) for part in getattr(proc, "args", ())),
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
        encoding="utf-8",
        errors="replace",
        start_new_session=True,
        env=dict(env) if env is not None else None,
    )
    return finish_bounded_process(
        proc,
        timeout=timeout,
        input_text=input_text,
        terminate_grace=terminate_grace,
        process_group_id=proc.pid,
    )
