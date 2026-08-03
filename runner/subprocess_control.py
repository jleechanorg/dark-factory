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


def as_bytes(value: str | bytes | None) -> bytes:
    if value is None:
        return b""
    if isinstance(value, bytes):
        return value
    return value.encode("utf-8")


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


def _merge_output_bytes(current: str | bytes | None, later: str | bytes | None) -> bytes:
    current_bytes = as_bytes(current)
    later_bytes = as_bytes(later)
    if not later_bytes or later_bytes == current_bytes:
        return current_bytes
    if later_bytes.startswith(current_bytes):
        return later_bytes
    if current_bytes.endswith(later_bytes):
        return current_bytes
    return current_bytes + later_bytes


@dataclass(frozen=True, slots=True)
class BoundedProcessResult:
    args: tuple[str, ...]
    returncode: int
    stdout: str
    stderr: str
    timed_out: bool


@dataclass(frozen=True, slots=True)
class BoundedProcessBytesResult:
    args: tuple[str, ...]
    returncode: int
    stdout: bytes
    stderr: bytes
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
    preserve_bytes: bool = False,
) -> BoundedProcessResult | BoundedProcessBytesResult:
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
        if preserve_bytes:
            stdout, stderr = as_bytes(initial_timeout.stdout), as_bytes(initial_timeout.stderr)
        else:
            stdout, stderr = as_text(initial_timeout.stdout), as_text(initial_timeout.stderr)
        _signal_process_group(pgid, signal.SIGTERM)
        deadline = time.monotonic() + max(0.0, terminate_grace)
        try:
            drained_stdout, drained_stderr = proc.communicate(timeout=terminate_grace)
            stdout, stderr = drained_stdout, drained_stderr
        except subprocess.TimeoutExpired as cleanup_timeout:
            if preserve_bytes:
                stdout = _merge_output_bytes(stdout, cleanup_timeout.stdout)
                stderr = _merge_output_bytes(stderr, cleanup_timeout.stderr)
            else:
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
            if preserve_bytes:
                stdout = _merge_output_bytes(stdout, final_timeout.stdout)
                stderr = _merge_output_bytes(stderr, final_timeout.stderr)
            else:
                stdout = _merge_output(stdout, final_timeout.stdout)
                stderr = _merge_output(stderr, final_timeout.stderr)
        except Exception:
            pass
    result_type = BoundedProcessBytesResult if preserve_bytes else BoundedProcessResult
    return result_type(
        args=tuple(str(part) for part in getattr(proc, "args", ())),
        returncode=int(proc.returncode if proc.returncode is not None else -1),
        stdout=as_bytes(stdout) if preserve_bytes else as_text(stdout),
        stderr=as_bytes(stderr) if preserve_bytes else as_text(stderr),
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


def run_bounded_process_bytes(
    args: Sequence[str],
    *,
    cwd: str | Path | None = None,
    timeout: float,
    env: Mapping[str, str] | None = None,
    terminate_grace: float = 5.0,
) -> BoundedProcessBytesResult:
    """Run an exact argv while preserving stdout/stderr bytes losslessly."""
    proc = subprocess.Popen(
        list(args),
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
        env=dict(env) if env is not None else None,
    )
    result = finish_bounded_process(
        proc,
        timeout=timeout,
        terminate_grace=terminate_grace,
        process_group_id=proc.pid,
        preserve_bytes=True,
    )
    assert isinstance(result, BoundedProcessBytesResult)
    return result
