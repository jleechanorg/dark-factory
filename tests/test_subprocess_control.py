from __future__ import annotations

import os
import subprocess
import sys
import time

import pytest


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


@pytest.mark.parametrize("lane", ["ao_spawn", "ao_send", "tool"])
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
