"""Pinned Codex runtime and shared-cache contract tests."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest


PINNED_VERSION = "0.146.0"
NODE_VERSION = "v22.22.0"


def _write_runtime(
    home: Path,
    *,
    package_version: str = PINNED_VERSION,
    cache_version: str = PINNED_VERSION,
    include_reasoning_summaries: bool = True,
) -> tuple[Path, Path, Path]:
    package_root = (
        home
        / ".nvm"
        / "versions"
        / "node"
        / NODE_VERSION
        / "lib"
        / "node_modules"
        / "@openai"
        / "codex"
    )
    script = package_root / "bin" / "codex.js"
    script.parent.mkdir(parents=True)
    script.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    script.chmod(0o755)
    (package_root / "package.json").write_text(
        json.dumps(
            {
                "name": "@openai/codex",
                "version": package_version,
                "bin": {"codex": "bin/codex.js"},
            }
        ),
        encoding="utf-8",
    )
    executable = home / ".nvm" / "versions" / "node" / NODE_VERSION / "bin" / "codex"
    executable.parent.mkdir(parents=True)
    executable.symlink_to(Path("../lib/node_modules/@openai/codex/bin/codex.js"))

    model = {"slug": "fake-model"}
    if include_reasoning_summaries:
        model["supports_reasoning_summaries"] = True
    cache = home / ".codex" / "models_cache.json"
    cache.parent.mkdir()
    cache.write_text(
        json.dumps({"client_version": cache_version, "models": [model]}),
        encoding="utf-8",
    )
    return executable, package_root / "package.json", cache


def test_aligned_fixture_resolves_one_absolute_node22_executable(tmp_path: Path) -> None:
    from runner.codex_runtime import resolve_codex_runtime

    executable, _, cache = _write_runtime(tmp_path)

    runtime = resolve_codex_runtime(
        home=tmp_path,
        cache_path=cache,
        competing_package_paths=(),
    )

    assert runtime.executable == executable
    assert runtime.executable.is_absolute()
    assert runtime.version == PINNED_VERSION
    assert NODE_VERSION in runtime.executable.parts


@pytest.mark.parametrize(
    ("package_version", "cache_version", "include_field", "match"),
    [
        ("0.144.5", PINNED_VERSION, True, "installed Codex version"),
        (PINNED_VERSION, "0.144.5", True, "cache client_version"),
        (PINNED_VERSION, PINNED_VERSION, False, "supports_reasoning_summaries"),
    ],
)
def test_skew_rejects_without_changing_cache(
    tmp_path: Path,
    package_version: str,
    cache_version: str,
    include_field: bool,
    match: str,
) -> None:
    from runner.codex_runtime import CodexRuntimeError, resolve_codex_runtime

    _, _, cache = _write_runtime(
        tmp_path,
        package_version=package_version,
        cache_version=cache_version,
        include_reasoning_summaries=include_field,
    )
    before_bytes = cache.read_bytes()
    before_stat = cache.stat()

    with pytest.raises(CodexRuntimeError, match=match):
        resolve_codex_runtime(
            home=tmp_path,
            cache_path=cache,
            competing_package_paths=(),
        )

    after_stat = cache.stat()
    assert cache.read_bytes() == before_bytes
    assert after_stat.st_mtime_ns == before_stat.st_mtime_ns


def test_competing_package_skew_rejects(tmp_path: Path) -> None:
    from runner.codex_runtime import CodexRuntimeError, resolve_codex_runtime

    _, _, cache = _write_runtime(tmp_path)
    competitor = tmp_path / "homebrew" / "@openai" / "codex" / "package.json"
    competitor.parent.mkdir(parents=True)
    competitor.write_text(
        json.dumps({"name": "@openai/codex", "version": "0.144.5"}),
        encoding="utf-8",
    )

    with pytest.raises(CodexRuntimeError, match="competing Codex version"):
        resolve_codex_runtime(
            home=tmp_path,
            cache_path=cache,
            competing_package_paths=(competitor,),
        )


def test_resolver_never_executes_codex_for_version(tmp_path: Path, monkeypatch) -> None:
    from runner import codex_runtime

    _, _, cache = _write_runtime(tmp_path)

    def _forbidden(*args, **kwargs):
        raise AssertionError("resolver must not start a process")

    monkeypatch.setattr(subprocess, "run", _forbidden)

    runtime = codex_runtime.resolve_codex_runtime(
        home=tmp_path,
        cache_path=cache,
        competing_package_paths=(),
    )
    assert runtime.version == PINNED_VERSION
