from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent


def test_public_benchmark_files_do_not_leak_sealed_details():
    proc = subprocess.run(
        [sys.executable, str(ROOT / "benchmarks" / "scripts" / "check_boundary.py")],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )

    assert proc.returncode == 0, proc.stdout + proc.stderr

