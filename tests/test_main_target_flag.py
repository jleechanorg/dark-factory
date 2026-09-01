from __future__ import annotations

import json
import pathlib
import sys
import types

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.__main__ import main  # noqa: E402


def _fake_history_record(outcome: str = "success"):
    return types.SimpleNamespace(
        node="start", outcome=outcome, output_preview="ok", metadata={}
    )


def _stub_run(monkeypatch, captured: dict):
    def fake_run(graph, ctx, **kwargs):
        captured["ctx"] = ctx
        return [_fake_history_record()]

    monkeypatch.setattr("runner.__main__.run", fake_run)


class TestTargetFlagResolution:
    def test_unresolvable_target_refuses_to_start(self, tmp_path, capsys):
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--target", "this is not resolvable to anything at all",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "--target" in err

    def test_defined_not_resolvable_scheme_refuses_to_start(self, tmp_path, capsys):
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--target", "bead://abc123",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "not resolvable in v1" in err

    def test_valid_file_target_sets_ctx_state_and_makes_goal_optional(
        self, tmp_path, monkeypatch, capsys
    ):
        target_file = tmp_path / "spec.md"
        target_file.write_text("spec body")
        captured: dict = {}
        _stub_run(monkeypatch, captured)

        rc = main([
            "--pipeline", "pipelines/factory/hello.dot",
            "--target", str(target_file),
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--no-perf-log",
            "--evidence-bundle", str(tmp_path / "evidence"),
        ])

        assert rc == 0
        ctx = captured["ctx"]
        assert ctx.state["target"].startswith("file://")
        assert ctx.state["_df_target_mode"] == "true"
        assert ctx.goal == ""

    def test_scheme_uri_target_delegates_to_strict_parse(
        self, tmp_path, monkeypatch
    ):
        target_file = tmp_path / "spec.md"
        target_file.write_text("body")
        import hashlib
        digest = hashlib.sha256(b"body").hexdigest()
        captured: dict = {}
        _stub_run(monkeypatch, captured)

        rc = main([
            "--pipeline", "pipelines/factory/hello.dot",
            "--target", f"file://{target_file}@sha256:{digest}",
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--no-perf-log",
            "--evidence-bundle", str(tmp_path / "evidence"),
        ])

        assert rc == 0
        assert captured["ctx"].state["target"] == f"file://{target_file.resolve()}@sha256:{digest}"

    def test_goal_still_required_without_target(self, tmp_path, capsys):
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "--goal is required" in err

    def test_target_and_goal_are_mutually_exclusive(self, tmp_path, capsys):
        """D5 (v3.1 delta): combining --target and --goal is an argparse
        error — a target-mode verification run has no free-text goal."""
        target_file = tmp_path / "spec.md"
        target_file.write_text("body")
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--target", str(target_file),
                "--goal", "do something",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "mutually exclusive" in err
