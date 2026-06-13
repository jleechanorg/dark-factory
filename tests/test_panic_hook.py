"""Unit tests for runner.panic_hook.

Coverage:
  * Artifact creation + JSON validity
  * Secret-stripping in env_filtered (positive + negative)
  * run_id extraction from the documented --state KEY=VALUE pattern
  * Machine-readable contract (all required fields present + typed)
  * Bash-mode entry point (no Python traceback → synthesized message)
  * Fail-safe contract (the hook itself never crashes)
"""

from __future__ import annotations

import dataclasses
import json
import os
import pathlib
import subprocess
import sys

import pytest

from runner import panic_hook


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture
def tmp_panic_dir(tmp_path: pathlib.Path, monkeypatch) -> pathlib.Path:
    """Give every test its own PANIC_DIR to avoid touching ~/.dark-factory."""
    target = tmp_path / "panics"
    target.mkdir(parents=True, exist_ok=True)
    # The function reads PANIC_DIR via the module constant, not env,
    # so we patch the module attribute directly.
    monkeypatch.setattr(panic_hook, "PANIC_DIR", target)
    return target


def _sample_env() -> dict[str, str]:
    return {
        "DARK_FACTORY_HOME": "/Users/jleechan/projects/dark-factory",
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "OPENAI_API_KEY": "sk-test-redacted",
        "GITHUB_TOKEN": "ghp_test_redacted",
        "ANTHROPIC_API_KEY": "sk-ant-redacted",
        "DARK_FACTORY_HOLDOUTS": "/Users/jleechan/projects/dark-factory-holdouts",
        "MY_PASSWORD": "hunter2",
        "DATABASE_URL": "postgres://user@host/db",
    }


# ---------------------------------------------------------------------------
# Artifact creation
# ---------------------------------------------------------------------------

def test_write_crash_artifact_creates_file_in_panic_dir(tmp_panic_dir: pathlib.Path) -> None:
    artifact = panic_hook.write_crash_artifact(
        traceback_str="Traceback (most recent call last):\n  File 'x.py', line 1\nValueError: boom",
        argv=["dark-factory", "--backend", "claude", "--pipeline", "foo.dot"],
        cwd="/tmp",
        run_id="abc-123",
        env_filtered={"DARK_FACTORY_HOME": "/Users/jleechan/projects/dark-factory"},
        exit_code=1,
    )
    assert artifact.exists(), f"artifact {artifact} was not created"
    parsed = json.loads(artifact.read_text())
    assert parsed["run_id"] == "abc-123"
    assert parsed["argv"][0] == "dark-factory"
    assert parsed["exit_code"] == 1
    assert "ValueError: boom" in parsed["traceback"]


def test_write_crash_artifact_filename_is_deterministic(tmp_panic_dir: pathlib.Path) -> None:
    argv = ["dark-factory", "--backend", "echo"]
    one = panic_hook.write_crash_artifact("tb", argv, "/tmp", None, {}, exit_code=1)
    # Two writes in the same second must not collide (timestamp + argv-hash).
    two = panic_hook.write_crash_artifact("tb", argv, "/tmp", None, {}, exit_code=1)
    assert one == two, "same argv should produce the same filename (no collisions)"


def test_write_crash_artifact_creates_directory_when_missing(tmp_path: pathlib.Path) -> None:
    target = tmp_path / "deep" / "nested" / "panics"
    artifact = panic_hook.write_crash_artifact(
        "tb", ["df"], str(tmp_path), None, {}, exit_code=1, panic_dir=target
    )
    assert artifact.exists()
    assert artifact.parent == target


# ---------------------------------------------------------------------------
# Secret stripping
# ---------------------------------------------------------------------------

def test_env_filtered_strips_secrets() -> None:
    env = _sample_env()
    safe = panic_hook.filter_env(env)
    for forbidden in ("OPENAI_API_KEY", "GITHUB_TOKEN", "ANTHROPIC_API_KEY", "MY_PASSWORD", "DARK_FACTORY_HOLDOUTS"):
        assert forbidden not in safe, f"secret variable {forbidden} leaked into filtered env"


def test_env_filtered_keeps_safe_vars() -> None:
    env = _sample_env()
    safe = panic_hook.filter_env(env)
    assert safe["DARK_FACTORY_HOME"] == "/Users/jleechan/projects/dark-factory"
    assert safe["PATH"].startswith("/usr/local/bin")
    # DATABASE_URL has no secret-like substring and should survive.
    assert "DATABASE_URL" in safe


def test_env_filtered_case_insensitive() -> None:
    env = {"my_key": "x", "API_token": "y", "gitHub_Key": "z", "safe_var": "keep"}
    safe = panic_hook.filter_env(env)
    assert "my_key" not in safe
    assert "API_token" not in safe
    assert "gitHub_Key" not in safe
    assert safe["safe_var"] == "keep"


def test_env_filtered_accepts_iterable_of_pairs() -> None:
    env = [("FOO", "bar"), ("TOKEN", "secret")]
    safe = panic_hook.filter_env(env)
    assert safe == {"FOO": "bar"}


# ---------------------------------------------------------------------------
# run_id extraction
# ---------------------------------------------------------------------------

def test_run_id_extracted_from_state_flag() -> None:
    argv = ["dark-factory", "--backend", "claude", "--state", "run_id=my-run-42", "--goal", "x"]
    assert panic_hook.extract_run_id_from_argv(argv) == "my-run-42"


def test_run_id_absent_returns_none() -> None:
    argv = ["dark-factory", "--backend", "claude", "--goal", "x"]
    assert panic_hook.extract_run_id_from_argv(argv) is None


def test_run_id_in_artifact_matches_argv(tmp_panic_dir: pathlib.Path) -> None:
    argv = ["dark-factory", "--state", "run_id=run-from-state", "--backend", "echo"]
    artifact = panic_hook.write_crash_artifact(
        "tb", argv, "/tmp", panic_hook.extract_run_id_from_argv(argv[1:]), {}, exit_code=1
    )
    parsed = json.loads(artifact.read_text())
    assert parsed["run_id"] == "run-from-state"


# ---------------------------------------------------------------------------
# Machine-readable contract
# ---------------------------------------------------------------------------

def test_crash_artifact_is_machine_readable(tmp_panic_dir: pathlib.Path) -> None:
    artifact = panic_hook.write_crash_artifact(
        traceback_str="boom",
        argv=["dark-factory", "--backend", "echo"],
        cwd="/tmp",
        run_id=None,
        env_filtered={"DARK_FACTORY_HOME": "/x"},
        exit_code=2,
    )
    raw = artifact.read_text()
    parsed = json.loads(raw)  # must be valid JSON
    required = {"ts", "run_id", "argv", "cwd", "traceback", "env_filtered", "exit_code"}
    assert required.issubset(parsed.keys()), f"missing fields: {required - set(parsed.keys())}"
    # Field types — strict, so future Healer changes break loudly.
    assert isinstance(parsed["ts"], str) and parsed["ts"].endswith("Z")
    assert parsed["run_id"] is None
    assert isinstance(parsed["argv"], list)
    assert isinstance(parsed["cwd"], str)
    assert isinstance(parsed["traceback"], str)
    assert isinstance(parsed["env_filtered"], dict)
    assert isinstance(parsed["exit_code"], int)


# ---------------------------------------------------------------------------
# CLI entry point (bash wrapper contract)
# ---------------------------------------------------------------------------

def test_main_returns_panic_exit_code() -> None:
    rc = panic_hook.main(["--exit-code", "7"])
    assert rc == panic_hook.PANIC_EXIT_CODE


def test_main_writes_artifact_for_bash_crash(tmp_path: pathlib.Path, monkeypatch) -> None:
    target = tmp_path / "bash-panics"
    target.mkdir(parents=True, exist_ok=True)
    bash_argv = json.dumps(["dark-factory", "--backend", "claude", "--pipeline", "missing.dot"])
    rc = panic_hook.main(
        [
            "--exit-code", "1",
            "--line", "42",
            "--bash-argv", bash_argv,
            "--panic-dir", str(target),
        ]
    )
    assert rc == panic_hook.PANIC_EXIT_CODE
    files = list(target.iterdir())
    assert len(files) == 1, f"expected exactly one artifact, got {files}"
    parsed = json.loads(files[0].read_text())
    assert parsed["exit_code"] == 1
    assert "Bash panic" in parsed["traceback"]
    assert "dark-factory" in parsed["argv"]


def test_cli_subprocess_writes_artifact(tmp_path: pathlib.Path) -> None:
    """End-to-end: invoking `python -m runner.panic_hook` from a subprocess
    must produce a valid artifact in the requested panic-dir, NOT in
    the user's real ~/.dark-factory/panics/. This is the bash wrapper
    contract — the artifact must land in the requested directory."""
    target = tmp_path / "subproc-panics"
    target.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    env["PYTHONPATH"] = str(repo_root) + os.pathsep + env.get("PYTHONPATH", "")
    bash_argv = json.dumps(["dark-factory", "--backend", "echo"])
    proc = subprocess.run(
        [
            sys.executable, "-m", "runner.panic_hook",
            "--exit-code", "1",
            "--line", "10",
            "--bash-argv", bash_argv,
            "--panic-dir", str(target),
        ],
        capture_output=True,
        text=True,
        check=False,
        env=env,
        cwd=str(tmp_path),
    )
    assert proc.returncode == panic_hook.PANIC_EXIT_CODE, proc.stderr
    # Artifact must be in the requested tmp_path, not ~/.dark-factory.
    files = list(target.iterdir())
    assert len(files) == 1, f"expected exactly one artifact in {target}, got {files}"
    parsed = json.loads(files[0].read_text())
    assert parsed["exit_code"] == 1
    assert "dark-factory" in parsed["argv"]


# ---------------------------------------------------------------------------
# Fail-safe contract
# ---------------------------------------------------------------------------

def test_hook_does_not_crash_on_unwritable_dir(tmp_path: pathlib.Path) -> None:
    """If PANIC_DIR is a path inside a read-only file, the hook must
    NOT raise — it returns the intended path so the caller can log it."""
    read_only = tmp_path / "blocker"
    read_only.write_text("not a directory")
    bogus_parent = read_only / "panics"
    # Should not raise; should return a path under bogus_parent.
    out = panic_hook.write_crash_artifact(
        "tb", ["df"], "/tmp", None, {}, exit_code=1, panic_dir=bogus_parent
    )
    assert out.parent == bogus_parent  # path is returned even if write fails
