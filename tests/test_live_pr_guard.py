"""dark-factory#828 item (d): live/open-PR target refusal.

Real incident: a review-only run against a workdir whose branch had a
LIVE open PR silently committed and pushed to it. These tests cover
runner/live_pr_guard.py's detection contract (fails open on uncertainty,
refuses only on a confirmed OPEN PR) and the CLI wiring in
runner/__main__.py.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import types

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner.live_pr_guard import detect_live_pr  # noqa: E402
from runner.__main__ import main  # noqa: E402


def _fake_proc(returncode: int, stdout: str = "", stderr: str = "") -> subprocess.CompletedProcess:
    return subprocess.CompletedProcess(args=["gh"], returncode=returncode, stdout=stdout, stderr=stderr)


class TestDetectLivePr:
    def test_open_pr_detected(self, monkeypatch, tmp_path):
        def fake_run(*args, **kwargs):
            return _fake_proc(0, json.dumps({"number": 9583, "url": "https://x/9583", "state": "OPEN"}))

        monkeypatch.setattr(subprocess, "run", fake_run)
        result = detect_live_pr(tmp_path)
        assert result is not None
        assert result["number"] == 9583
        assert result["state"] == "OPEN"

    def test_closed_pr_is_not_live(self, monkeypatch, tmp_path):
        def fake_run(*args, **kwargs):
            return _fake_proc(0, json.dumps({"number": 1, "url": "https://x/1", "state": "CLOSED"}))

        monkeypatch.setattr(subprocess, "run", fake_run)
        assert detect_live_pr(tmp_path) is None

    def test_merged_pr_is_not_live(self, monkeypatch, tmp_path):
        def fake_run(*args, **kwargs):
            return _fake_proc(0, json.dumps({"number": 1, "url": "https://x/1", "state": "MERGED"}))

        monkeypatch.setattr(subprocess, "run", fake_run)
        assert detect_live_pr(tmp_path) is None

    def test_no_pr_for_branch_fails_open(self, monkeypatch, tmp_path):
        """`gh pr view` exits non-zero when the branch has no PR at all."""
        def fake_run(*args, **kwargs):
            return _fake_proc(1, "", "no pull requests found for branch")

        monkeypatch.setattr(subprocess, "run", fake_run)
        assert detect_live_pr(tmp_path) is None

    def test_gh_not_installed_fails_open(self, monkeypatch, tmp_path):
        def fake_run(*args, **kwargs):
            raise OSError("gh: command not found")

        monkeypatch.setattr(subprocess, "run", fake_run)
        assert detect_live_pr(tmp_path) is None

    def test_gh_timeout_fails_open(self, monkeypatch, tmp_path):
        def fake_run(*args, **kwargs):
            raise subprocess.TimeoutExpired(cmd="gh", timeout=10)

        monkeypatch.setattr(subprocess, "run", fake_run)
        assert detect_live_pr(tmp_path) is None

    def test_malformed_json_fails_open(self, monkeypatch, tmp_path):
        def fake_run(*args, **kwargs):
            return _fake_proc(0, "not json at all")

        monkeypatch.setattr(subprocess, "run", fake_run)
        assert detect_live_pr(tmp_path) is None

    def test_empty_workdir_fails_open(self):
        assert detect_live_pr(None) is None


def _fake_history_record(outcome: str = "success"):
    return types.SimpleNamespace(node="start", outcome=outcome, output_preview="ok", metadata={})


def _stub_run(monkeypatch, captured: dict):
    def fake_run(graph, ctx, **kwargs):
        captured["ctx"] = ctx
        return [_fake_history_record()]

    monkeypatch.setattr("runner.__main__.run", fake_run)


class TestCliLivePrRefusal:
    def test_open_pr_target_refuses_without_flag(self, monkeypatch, tmp_path, capsys):
        monkeypatch.setattr(
            "runner.live_pr_guard.detect_live_pr",
            lambda workdir, timeout=10: {"number": 9583, "url": "https://x/9583", "state": "OPEN"},
        )
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--goal", "review only, do not change code",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "9583" in err
        assert "--allow-live-pr-target" in err

    def test_open_pr_target_proceeds_with_flag(self, monkeypatch, tmp_path, capsys):
        monkeypatch.setattr(
            "runner.live_pr_guard.detect_live_pr",
            lambda workdir, timeout=10: {"number": 9583, "url": "https://x/9583", "state": "OPEN"},
        )
        captured: dict = {}
        _stub_run(monkeypatch, captured)
        rc = main([
            "--pipeline", "pipelines/factory/hello.dot",
            "--goal", "review only, do not change code",
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--allow-live-pr-target",
            "--no-perf-log",
        ])
        assert rc == 0
        assert "ctx" in captured

    def test_no_live_pr_proceeds_without_flag(self, monkeypatch, tmp_path, capsys):
        monkeypatch.setattr("runner.live_pr_guard.detect_live_pr", lambda workdir, timeout=10: None)
        captured: dict = {}
        _stub_run(monkeypatch, captured)
        rc = main([
            "--pipeline", "pipelines/factory/hello.dot",
            "--goal", "a normal feature run",
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--no-perf-log",
        ])
        assert rc == 0
        assert "ctx" in captured
