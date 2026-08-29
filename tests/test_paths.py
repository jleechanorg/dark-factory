"""Tests for DARK_FACTORY_HOME path resolution."""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.paths import resolve_factory_path, resolve_pipeline_path


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


# --- resolve_pipeline_path: target-repo subdir convention -------------------


def _write(path: pathlib.Path, body: str = "digraph x {}") -> pathlib.Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    return path


def test_resolve_pipeline_absolute_path_wins(tmp_path):
    absolute = _write(tmp_path / "abs.dot")
    # An absolute path is always returned as-is (even if no file lives at
    # other candidate locations — the resolver never re-routes absolute paths).
    resolved = resolve_pipeline_path(absolute, workdir=tmp_path)
    assert resolved == absolute.resolve()


def test_resolve_pipeline_bare_filename_finds_subdir(tmp_path, monkeypatch):
    """Bare filename resolves to <workdir>/dark-factory/pipelines/<name>."""
    subdir = tmp_path / "dark-factory" / "pipelines"
    pipeline = _write(subdir / "pr_gates_split_cs.dot")

    # cwd deliberately different from workdir so the resolver cannot fall
    # back to a cwd-relative lookup.
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)

    resolved = resolve_pipeline_path("pr_gates_split_cs.dot", workdir=tmp_path)
    assert resolved == pipeline.resolve()


def test_resolve_pipeline_under_workdir_first(tmp_path):
    """A file at <workdir>/<name> beats the dark-factory/pipelines/ fallback."""
    workdir_file = _write(tmp_path / "foo.dot")
    subdir_file = _write(tmp_path / "dark-factory" / "pipelines" / "foo.dot")

    resolved = resolve_pipeline_path("foo.dot", workdir=tmp_path)
    assert resolved == workdir_file.resolve()
    assert resolved != subdir_file.resolve()


def test_resolve_pipeline_structured_path_not_rewritten(tmp_path, monkeypatch):
    """A path with separators is never rewritten via the subdir convention."""
    # Only the dark-factory/pipelines/<name> candidate exists. A structured
    # path like 'pipelines/factory/gates.dot' must NOT match
    # <workdir>/dark-factory/pipelines/pipelines/factory/gates.dot.
    subdir_strict = _write(
        tmp_path / "dark-factory" / "pipelines" / "pipelines" / "factory" / "gates.dot"
    )
    # Realistic factory-home lookup should pick up the real pipeline.
    home = tmp_path / "factory_home"
    home_pipeline = _write(home / "pipelines" / "factory" / "gates.dot")

    # chdir away from the project root so the fall-through `resolve_factory_path`
    # does not pick up the real `pipelines/factory/gates.dot` from cwd.
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)

    monkeypatch.setenv("DARK_FACTORY_HOME", str(home))

    resolved = resolve_pipeline_path(
        "pipelines/factory/gates.dot",
        workdir=tmp_path,
    )
    # We never re-route structured paths into the subdir; the subdir location
    # did NOT match the structured input.
    assert resolved != subdir_strict.resolve()
    # And the structured input is found under factory home.
    assert resolved == home_pipeline.resolve()


def test_resolve_pipeline_falls_through_to_factory_home(tmp_path, monkeypatch):
    """When workdir has no match, the resolver tries $DARK_FACTORY_HOME."""
    home = tmp_path / "factory_home"
    home_pipeline = _write(home / "pipelines" / "factory" / "gates.dot")

    monkeypatch.setenv("DARK_FACTORY_HOME", str(home))

    # chdir away from the project root so the fall-through does not see
    # the real `pipelines/factory/gates.dot` from cwd.
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)

    resolved = resolve_pipeline_path("pipelines/factory/gates.dot", workdir=tmp_path)
    assert resolved == home_pipeline.resolve()


def test_resolve_pipeline_no_match_returns_resolved_candidate(tmp_path, monkeypatch):
    """Final fallback is the same as resolve_factory_path: return a resolved
    path even if nothing matches (the caller will fail-closed with a clear
    'file not found' error)."""
    monkeypatch.setenv("DARK_FACTORY_HOME", "")
    monkeypatch.chdir(tmp_path)
    # No files anywhere — both workdir, subdir, and factory home are empty.
    resolved = resolve_pipeline_path("does_not_exist.dot", workdir=tmp_path)
    assert resolved == (tmp_path / "does_not_exist.dot").resolve()


def test_resolve_pipeline_short_alias_ready_finds_slim_ready(tmp_path, monkeypatch):
    """Short alias 'ready' resolves to pipelines/slim/ready.dot under factory home."""
    home = tmp_path / "factory_home"
    ready_dot = _write(home / "pipelines" / "slim" / "ready.dot")
    monkeypatch.setenv("DARK_FACTORY_HOME", str(home))

    target_repo = tmp_path / "target_repo"
    target_repo.mkdir()
    monkeypatch.chdir(target_repo)

    resolved = resolve_pipeline_path("ready", workdir=target_repo)
    assert resolved == ready_dot.resolve()

    resolved_ext = resolve_pipeline_path("ready.dot", workdir=target_repo)
    assert resolved_ext == ready_dot.resolve()


def test_resolve_pipeline_short_alias_target_repo_precedence(tmp_path, monkeypatch):
    """Target repo's local ready.dot or dark-factory/pipelines/ready.dot takes precedence over factory alias."""
    home = tmp_path / "factory_home"
    factory_ready = _write(home / "pipelines" / "slim" / "ready.dot", "digraph factory_ready {}")
    monkeypatch.setenv("DARK_FACTORY_HOME", str(home))

    # 1. Target repo dark-factory/pipelines/ready.dot beats factory default
    target_repo = tmp_path / "target_repo"
    repo_subdir_ready = _write(target_repo / "dark-factory" / "pipelines" / "ready.dot", "digraph repo_ready {}")
    monkeypatch.chdir(target_repo)

    resolved = resolve_pipeline_path("ready", workdir=target_repo)
    assert resolved == repo_subdir_ready.resolve()
    assert resolved != factory_ready.resolve()

    # 2. Target repo root ready.dot beats target repo dark-factory/pipelines/ready.dot
    repo_root_ready = _write(target_repo / "ready.dot", "digraph root_ready {}")
    resolved2 = resolve_pipeline_path("ready", workdir=target_repo)
    assert resolved2 == repo_root_ready.resolve()
    assert resolved2 != repo_subdir_ready.resolve()
