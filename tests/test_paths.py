"""Tests for DARK_FACTORY_HOME path resolution."""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.paths import resolve_factory_path


def test_resolve_pipeline_under_factory_home(monkeypatch, tmp_path):
    home = tmp_path / "factory"
    pipeline = home / "pipelines" / "foo.dot"
    pipeline.parent.mkdir(parents=True)
    pipeline.write_text('digraph foo { start [shape=Mdiamond] exit [shape=Msquare] start -> exit }')

    monkeypatch.setenv("DARK_FACTORY_HOME", str(home))
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)

    resolved = resolve_factory_path(pathlib.Path("pipelines/foo.dot"))
    assert resolved == pipeline.resolve()
