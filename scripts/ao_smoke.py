"""Verification script: exercise the full --backend ao --ao-agent antigravity
codergen path end-to-end with a fake `ao` on PATH. The fake `ao` returns
"ready" on `ao status` so the wait_idle helper completes immediately.

Run from the worktree root with the shared venv:
    /home/jleechan/projects/dark-factory/.venv/bin/python scripts/ao_smoke.py

Exits 0 on success, non-zero on failure. Prints structured evidence
(JSON) suitable for inclusion in the PR evidence gist.
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile

WORKTREE = pathlib.Path("/home/jleechan/.worktrees/dark-factory/df-78")
VENV = pathlib.Path("/home/jleechan/projects/dark-factory/.venv/bin/python")


def main() -> int:
    shim_dir = tempfile.mkdtemp(prefix="ao-smoke-")
    shim = pathlib.Path(shim_dir) / "ao"
    shim.write_text(
        "#!/bin/sh\n"
        'case "$1" in\n'
        "  spawn)\n"
        '    echo "SESSION=fake-smoke-session"\n'
        '    echo "Worktree: /tmp/fake-smoke-worktree"\n'
        "    exit 0\n"
        "    ;;\n"
        "  status)\n"
        '    echo \'[{"name": "fake-smoke-session", "activity": "exited"}]\'\n'
        "    exit 0\n"
        "    ;;\n"
        "  send)\n"
        "    exit 0\n"
        "    ;;\n"
        "  *)\n"
        '    echo "fake ao: unknown subcommand $1" 1>&2\n'
        "    exit 2\n"
        "    ;;\n"
        "esac\n"
    )
    shim.chmod(shim.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    pipeline = WORKTREE / "pipelines" / "factory" / "hello.dot"
    workdir = tempfile.mkdtemp(prefix="ao-smoke-target-")
    pathlib.Path(workdir, "README.md").write_text("# smoke\n")

    # Build a minimal pipeline that exercises the codergen path with
    # --backend ao --ao-agent antigravity. Drop the _base.dot include so we
    # don't pull in the unreachable explore_* cluster.
    minimal = pathlib.Path(workdir, "smoke.dot")
    minimal.write_text(
        "digraph smoke {\n"
        '  graph [goal="ao backend smoke test"]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  exit  [shape=Msquare,  label="Exit"]\n'
        '  work [type="codergen", label="Work", prompt="@prompts/hello/plan.md"]\n'
        "  start -> work -> exit\n"
        "}\n"
    )

    env = dict(os.environ)
    env["PATH"] = f"{shim_dir}:{env.get('PATH', '')}"

    cmd = [
        str(VENV),
        "-m",
        "runner",
        "--pipeline",
        str(minimal),
        "--goal",
        "ao backend smoke test",
        "--backend",
        "ao",
        "--ao-agent",
        "antigravity",
        "--ao-project",
        "smoke-test",
        "--workdir",
        workdir,
        "--max-steps",
        "5",
    ]
    proc = subprocess.run(
        cmd, cwd=str(WORKTREE), capture_output=True, text=True, env=env, timeout=120
    )
    print(
        json.dumps(
            {
                "rc": proc.returncode,
                "stdout_tail": proc.stdout[-1500:],
                "stderr_tail": proc.stderr[-1500:],
            },
            indent=2,
        )
    )
    return proc.returncode


if __name__ == "__main__":
    sys.exit(main())
