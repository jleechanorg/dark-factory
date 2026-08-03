from __future__ import annotations

import os
import subprocess
import sys
import time

import pytest


def test_bounded_process_bytes_preserves_non_utf8_streams(tmp_path) -> None:
    from runner.subprocess_control import run_bounded_process_bytes

    result = run_bounded_process_bytes(
        [
            sys.executable,
            "-c",
            (
                "import os; "
                "os.write(1, b'out\\x00\\xff'); "
                "os.write(2, b'err\\x80\\n')"
            ),
        ],
        cwd=tmp_path,
        timeout=5,
    )

    assert result.returncode == 0
    assert result.timed_out is False
    assert result.stdout == b"out\x00\xff"
    assert result.stderr == b"err\x80\n"


def test_finish_bounded_process_decodes_timeout_byte_streams() -> None:
    from runner.subprocess_control import finish_bounded_process

    class FakeProcess:
        pid = 999_999_999
        args = ["codex"]
        returncode = -15
        calls = 0

        def communicate(self, input=None, timeout=None):
            self.calls += 1
            if self.calls == 1:
                raise subprocess.TimeoutExpired(
                    self.args,
                    timeout,
                    output=b"partial stdout",
                    stderr=b"partial stderr",
                )
            return b"partial stdout", b"partial stderr"

    result = finish_bounded_process(FakeProcess(), timeout=0.01)

    assert result.timed_out is True
    assert result.stdout == "partial stdout"
    assert result.stderr == "partial stderr"


def test_finish_bounded_process_preserves_cumulative_timeout_bytes(monkeypatch) -> None:
    from runner.subprocess_control import finish_bounded_process

    class FakeProcess:
        pid = 999_999_998
        args = ["codex"]
        returncode = -9
        calls = 0

        def communicate(self, input=None, timeout=None):
            self.calls += 1
            outputs = (
                (b"one", b"err-one"),
                (b"one-two", b"err-one-two"),
                (b"one-two-three", b"err-one-two-three"),
            )
            stdout, stderr = outputs[self.calls - 1]
            raise subprocess.TimeoutExpired(
                self.args, timeout, output=stdout, stderr=stderr
            )

    monkeypatch.setattr("runner.subprocess_control._process_group_exists", lambda pgid: False)
    result = finish_bounded_process(FakeProcess(), timeout=0.01, terminate_grace=0.01)

    assert result.timed_out is True
    assert result.stdout == "one-two-three"
    assert result.stderr == "err-one-two-three"


def test_bounded_process_timeout_kills_and_reaps_grandchild(tmp_path) -> None:
    from runner.subprocess_control import run_bounded_process

    child_pid_path = tmp_path / "child.pid"
    script = (
        "import subprocess,sys,time\n"
        "child=subprocess.Popen([sys.executable,'-c',"
        "'import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(30)'])\n"
        f"open({str(child_pid_path)!r},'w').write(str(child.pid))\n"
        "print('ready', flush=True)\n"
        "time.sleep(30)\n"
    )

    result = run_bounded_process(
        [sys.executable, "-c", script],
        cwd=tmp_path,
        timeout=0.2,
        terminate_grace=0.2,
    )

    assert result.timed_out is True
    assert "ready" in result.stdout
    child_pid = int(child_pid_path.read_text())
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        try:
            os.kill(child_pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.05)
    else:
        subprocess.run(["kill", "-KILL", str(child_pid)], check=False)
        raise AssertionError(f"grandchild {child_pid} survived bounded cleanup")


def test_bounded_process_kills_term_ignoring_grandchild_after_pipes_close(tmp_path) -> None:
    from runner.subprocess_control import run_bounded_process

    child_pid_path = tmp_path / "detached-child.pid"
    child_code = (
        "import signal,time; "
        "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
        "time.sleep(30)"
    )
    parent_code = (
        "import subprocess,sys,time\n"
        f"child=subprocess.Popen([sys.executable,'-c',{child_code!r}], "
        "stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n"
        f"open({str(child_pid_path)!r},'w').write(str(child.pid))\n"
        "print('ready', flush=True)\n"
        "time.sleep(30)\n"
    )

    result = run_bounded_process(
        [sys.executable, "-c", parent_code],
        cwd=tmp_path,
        timeout=0.2,
        terminate_grace=0.2,
    )

    assert result.timed_out is True
    child_pid = int(child_pid_path.read_text())
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        try:
            os.kill(child_pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.05)
    else:
        subprocess.run(["kill", "-KILL", str(child_pid)], check=False)
        raise AssertionError(f"closed-pipe grandchild {child_pid} survived cleanup")


@pytest.mark.parametrize("lane", ["ao_spawn", "ao_send", "ao_status", "tool"])
def test_live_timeout_lanes_use_bounded_helper_as_terminal_error(
    lane, tmp_path, monkeypatch
) -> None:
    from runner.handlers import Context, _codergen, _tool
    from runner.parser import Node
    from runner.subprocess_control import BoundedProcessResult

    calls: list[tuple[str, ...]] = []

    def fake_bounded(args, **kwargs):
        calls.append(tuple(str(part) for part in args))
        return BoundedProcessResult(
            args=tuple(str(part) for part in args),
            returncode=-15,
            stdout="partial stdout",
            stderr="partial stderr",
            timed_out=True,
        )

    def forbid_popen(*args, **kwargs):
        raise AssertionError("migrated timeout fixture attempted real Popen")

    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: list(args))
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})
    monkeypatch.setattr("runner.subprocess_control.subprocess.Popen", forbid_popen)

    if lane == "tool":
        monkeypatch.setattr("runner.handler_control.run_bounded_process", fake_bounded)
        result = _tool(
            Node(name="tool", attrs={"type": "tool", "command": "echo safe", "timeout": "1"}),
            Context(goal="timeout", workdir=tmp_path, backend="echo"),
        )
    elif lane == "ao_status":
        from runner.handler_ao import _ao_wait_idle

        monkeypatch.setattr("runner.handler_ao.run_bounded_process", fake_bounded)
        outcome = _ao_wait_idle(
            "existing-session", tmp_path, timeout=1, poll_interval=0
        )
        assert outcome == "timeout"
        assert len(calls) == 1
        return
    else:
        monkeypatch.setattr("runner.handler_codergen.run_bounded_process", fake_bounded)
        ctx = Context(goal="timeout", workdir=tmp_path, backend="ao")
        ctx.state["ao.project"] = "test-project"
        if lane == "ao_send":
            ctx.state["ao.session"] = "existing-session"
        result = _codergen(
            Node(name="worker", attrs={"type": "codergen", "backend": "ao", "timeout": "1"}),
            ctx,
        )

    assert len(calls) == 1
    assert result.outcome == "error"
    assert result.metadata["timed_out"] == "true"
    assert "partial stdout" in result.output
    assert "partial stderr" in result.output


def test_ao_wait_idle_filters_status_by_project(monkeypatch, tmp_path) -> None:
    from runner.handlers import _ao_wait_idle
    from runner.subprocess_control import BoundedProcessResult

    observed: dict[str, object] = {}

    def fake_bounded(args, **kwargs):
        observed["args"] = list(args)
        observed["timeout"] = kwargs["timeout"]
        return BoundedProcessResult(
            args=tuple(args),
            returncode=0,
            stdout='[{"name":"session-1","activity":"exited"}]',
            stderr="",
            timed_out=False,
        )

    ticks = iter((100.0, 101.0))
    monkeypatch.setattr("runner.handler_ao.time.monotonic", lambda: next(ticks))
    monkeypatch.setattr("runner.handler_ao.run_bounded_process", fake_bounded)

    result = _ao_wait_idle(
        "session-1", tmp_path, timeout=300, project="project-7"
    )

    assert result == "exited"
    assert observed["args"] == ["ao", "status", "-p", "project-7", "--json"]
    assert observed["timeout"] == 180


def test_ao_wait_idle_clamps_poll_and_sleep_to_remaining_deadline(
    monkeypatch, tmp_path
) -> None:
    from runner.handlers import _ao_wait_idle
    from runner.subprocess_control import BoundedProcessResult

    poll_timeouts: list[float] = []
    sleeps: list[float] = []

    def fake_bounded(args, **kwargs):
        poll_timeouts.append(kwargs["timeout"])
        return BoundedProcessResult(
            args=tuple(args),
            returncode=0,
            stdout='[{"name":"session-1","activity":"active"}]',
            stderr="",
            timed_out=False,
        )

    ticks = iter((100.0, 101.0, 103.0, 105.0))
    monkeypatch.setattr("runner.handler_ao.time.monotonic", lambda: next(ticks))
    monkeypatch.setattr("runner.handler_ao.time.sleep", sleeps.append)
    monkeypatch.setattr("runner.handler_ao.run_bounded_process", fake_bounded)

    result = _ao_wait_idle(
        "session-1", tmp_path, timeout=5, poll_interval=10
    )

    assert result == "timeout"
    assert poll_timeouts == [4.0]
    assert sleeps == [2.0]
    assert all(value > 0 for value in poll_timeouts)
