"""Tests for runner/_agy_safe.py — file-disjoint fix for jleechan-c5q."""

from __future__ import annotations

import subprocess
import sys

import pytest

from runner import _agy_safe


@pytest.fixture
def restore_popen():
    """Snapshot subprocess.Popen before each test and restore after."""
    original = subprocess.Popen
    yield original
    subprocess.Popen = original
    # Force the module's installed-flag back to False so each test gets a
    # clean slate (install() is idempotent but we want a true restart).
    _agy_safe._safe_popen_installed = False


def test_install_no_op_when_agy_on_path(monkeypatch, restore_popen):
    monkeypatch.setattr("runner._agy_safe.shutil.which", lambda name: "/usr/local/bin/agy")
    installed = _agy_safe.install()
    assert installed is False
    assert _agy_safe._safe_popen_installed is False
    # subprocess.Popen must be the real one
    assert subprocess.Popen is restore_popen


def test_install_patches_when_agy_missing(monkeypatch, restore_popen):
    monkeypatch.setattr("runner._agy_safe.shutil.which", lambda name: None)
    installed = _agy_safe.install()
    assert installed is True
    assert _agy_safe._safe_popen_installed is True
    # subprocess.Popen must now be the safe wrapper, NOT the real one
    assert subprocess.Popen is not restore_popen
    assert subprocess.Popen is _agy_safe._safe_popen


def test_stub_popen_for_agy_command(monkeypatch, restore_popen):
    monkeypatch.setattr("runner._agy_safe.shutil.which", lambda name: None)
    _agy_safe.install()
    proc = subprocess.Popen(
        ["agy", "--print", "hi"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    assert proc.returncode == 127
    assert proc.pid == -1
    assert proc.args == ["agy", "--print", "hi"]
    out, err = proc.communicate(timeout=10)
    assert out == "backend_missing=true\n"
    assert err == ""
    # poll/wait are no-ops returning the returncode
    assert proc.poll() == 127
    assert proc.wait() == 127
    proc.kill()  # must not raise


def test_real_popen_still_works_for_other_commands(monkeypatch, restore_popen):
    monkeypatch.setattr("runner._agy_safe.shutil.which", lambda name: None)
    _agy_safe.install()
    # Echo is universally available; the safe wrapper must delegate to real Popen
    proc = subprocess.Popen(
        ["echo", "hi"], stdout=subprocess.PIPE, text=True,
    )
    # The stub always has pid=-1; real Popen has a positive pid
    assert proc.pid != -1
    out, _ = proc.communicate(timeout=10)
    assert "hi" in out
    assert proc.returncode == 0


def test_install_idempotent(monkeypatch, restore_popen):
    monkeypatch.setattr("runner._agy_safe.shutil.which", lambda name: None)
    first = _agy_safe.install()
    second = _agy_safe.install()
    assert first is True
    assert second is True
    # Still patched
    assert subprocess.Popen is not restore_popen


def test_cli_module_runs(tmp_path):
    """Smoke: ``python -m runner._agy_safe`` exits 0 and reports state."""
    # Use the project venv python explicitly; tmp_path is unused but keeps
    # pytest from collecting this as a no-op fixture in some configurations.
    _ = tmp_path
    result = subprocess.run(
        [sys.executable, "-m", "runner._agy_safe"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stderr
    assert "patch_installed=" in result.stdout
    # The state depends on the host (is agy installed?) so just assert
    # the marker is present and the value is a valid boolean.
    line = result.stdout.strip()
    assert line.endswith("True") or line.endswith("False")
