"""Tests for runner outage smoke check script (Lane E/F remediation Candidate A).

Surfaces runner outage earlier to avoid prolonged UNSTABLE stalls and --admin merges.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
SCRIPT_PATH = REPO_ROOT / "scripts" / "check_runner_outage.py"
SH_SCRIPT_PATH = REPO_ROOT / "scripts" / "check_runner_outage.sh"


def test_script_files_exist() -> None:
    assert SCRIPT_PATH.exists(), f"Missing Python script at {SCRIPT_PATH}"
    assert SH_SCRIPT_PATH.exists(), f"Missing Shell script at {SH_SCRIPT_PATH}"


def test_parse_runners_count_online() -> None:
    from scripts.check_runner_outage import count_online_runners

    payload = {
        "total_count": 2,
        "runners": [
            {"id": 1, "name": "runner-1", "status": "online", "busy": False},
            {"id": 2, "name": "runner-2", "status": "offline", "busy": False},
        ],
    }
    assert count_online_runners(payload) == 1


def test_parse_runners_count_all_offline() -> None:
    from scripts.check_runner_outage import count_online_runners

    payload = {
        "total_count": 2,
        "runners": [
            {"id": 1, "name": "runner-1", "status": "offline", "busy": False},
            {"id": 2, "name": "runner-2", "status": "offline", "busy": False},
        ],
    }
    assert count_online_runners(payload) == 0


def test_runner_outage_check_cli_offline_returns_error(tmp_path: pathlib.Path) -> None:
    # Create fake gh that outputs 0 online runners
    fake_gh = tmp_path / "gh"
    fake_gh.write_text(
        "#!/bin/sh\n"
        "echo '{\"total_count\": 0, \"runners\": []}'\n"
    )
    fake_gh.chmod(0o755)

    import os
    env = dict(os.environ)
    env["PATH"] = f"{tmp_path}:{env.get('PATH', '')}"

    proc = subprocess.run(
        [sys.executable, str(SCRIPT_PATH), "--repo", "jleechanorg/dark-factory"],
        env=env,
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0
    assert "RUNNER OUTAGE" in proc.stdout or "RUNNER OUTAGE" in proc.stderr
