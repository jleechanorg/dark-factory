from __future__ import annotations

import os
import subprocess
import sys
import time


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
