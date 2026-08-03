"""Pinned Codex runtime and shared-cache contract tests."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from copy import deepcopy
from pathlib import Path

import pytest


PINNED_VERSION = "0.146.0"
NODE_VERSION = "v22.22.0"


def _release_model() -> dict:
    """ModelInfo serialized by the official rust-v0.146.0 test fixture.

    Source: openai/codex@e363b08, codex-rs/app-server/tests/common/models_cache.rs
    and codex-rs/protocol/src/openai_models.rs. The omitted
    supports_reasoning_summary_parameter field is the valid default-true form.
    """
    return {
        "slug": "fake-model",
        "display_name": "Fake model",
        "description": "release-authentic fixture",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "Balanced reasoning"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": True,
        "priority": 0,
        "additional_speed_tiers": [],
        "service_tiers": [],
        "availability_nux": None,
        "upgrade": None,
        "base_instructions": "base instructions",
        "include_skills_usage_instructions": False,
        "default_reasoning_summary": "auto",
        "support_verbosity": False,
        "default_verbosity": None,
        "apply_patch_tool_type": None,
        "web_search_tool_type": "text",
        "truncation_policy": {"mode": "bytes", "limit": 10_000},
        "supports_parallel_tool_calls": False,
        "supports_image_detail_original": False,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": ["text", "image"],
        "supports_search_tool": False,
        "use_responses_lite": False,
    }


def _write_runtime(
    home: Path,
    *,
    package_version: str = PINNED_VERSION,
    cache_version: str = PINNED_VERSION,
    model_overrides: dict | None = None,
    omit_model_fields: tuple[str, ...] = (),
    cache_overrides: dict | None = None,
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

    model = deepcopy(_release_model())
    model.update(model_overrides or {})
    for field in omit_model_fields:
        model.pop(field, None)
    cache_payload = {
        "fetched_at": "2026-08-03T00:00:00Z",
        "etag": None,
        "client_version": cache_version,
        "models": [model],
    }
    cache_payload.update(cache_overrides or {})
    cache = home / ".codex" / "models_cache.json"
    cache.parent.mkdir()
    cache.write_text(
        json.dumps(cache_payload),
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
    ("package_version", "cache_version", "match"),
    [
        ("0.144.5", PINNED_VERSION, "installed Codex version"),
        (PINNED_VERSION, "0.144.5", "cache client_version"),
    ],
)
def test_skew_rejects_without_changing_cache(
    tmp_path: Path,
    package_version: str,
    cache_version: str,
    match: str,
) -> None:
    from runner.codex_runtime import CodexRuntimeError, resolve_codex_runtime

    _, _, cache = _write_runtime(
        tmp_path,
        package_version=package_version,
        cache_version=cache_version,
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


@pytest.mark.parametrize(
    ("runtime_kwargs", "match"),
    [
        ({"cache_overrides": {"fetched_at": None}}, "fetched_at"),
        ({"cache_overrides": {"fetched_at": "not-rfc3339"}}, "fetched_at"),
        ({"omit_model_fields": ("display_name",)}, "display_name"),
        (
            {"model_overrides": {"supports_reasoning_summary_parameter": "yes"}},
            "supports_reasoning_summary_parameter",
        ),
    ],
)
def test_release_cache_schema_rejects_invalid_shapes_without_mutation(
    tmp_path: Path, runtime_kwargs: dict, match: str
) -> None:
    from runner.codex_runtime import CodexRuntimeError, resolve_codex_runtime

    _, _, cache = _write_runtime(tmp_path, **runtime_kwargs)
    before = cache.read_bytes()

    with pytest.raises(CodexRuntimeError, match=match):
        resolve_codex_runtime(
            home=tmp_path,
            cache_path=cache,
            competing_package_paths=(),
        )

    assert cache.read_bytes() == before


def test_obsolete_minimal_cache_shape_is_rejected(tmp_path: Path) -> None:
    from runner.codex_runtime import CodexRuntimeError, resolve_codex_runtime

    _, _, cache = _write_runtime(
        tmp_path,
        cache_overrides={
            "models": [
                {"slug": "old-model", "supports_reasoning_summaries": True}
            ]
        },
    )

    with pytest.raises(CodexRuntimeError, match="display_name"):
        resolve_codex_runtime(
            home=tmp_path,
            cache_path=cache,
            competing_package_paths=(),
        )


def test_explicit_false_reasoning_summary_parameter_is_valid(tmp_path: Path) -> None:
    from runner.codex_runtime import resolve_codex_runtime

    _, _, cache = _write_runtime(
        tmp_path,
        model_overrides={"supports_reasoning_summary_parameter": False},
    )

    assert resolve_codex_runtime(
        home=tmp_path,
        cache_path=cache,
        competing_package_paths=(),
    ).version == PINNED_VERSION


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


def test_worker_gate_shadows_controller_and_skeptic_share_one_executable(
    tmp_path: Path, monkeypatch
) -> None:
    from runner import codex_runtime
    from runner.handlers import _codergen
    from runner.handler_codergen import _start_shadow_codex_review
    from runner.handler_core import Context
    from runner.handler_dispatch import (
        _controller_codex_args,
        _gate_subprocess_args,
        _launch_shadow_gate_review,
    )
    from runner.parser import Node
    from runner.skeptic_gate_cli import _build_reviewer_cmd
    from runner.subprocess_control import BoundedProcessResult

    resolved = str(
        tmp_path / ".nvm/versions/node/v22.22.0/bin/codex"
    )
    monkeypatch.setattr(codex_runtime, "resolve_codex_executable", lambda: resolved)
    monkeypatch.setattr("runner.handlers._sandboxed_args", lambda args: args)
    monkeypatch.setattr(
        "runner.handlers._sandboxed_args_for_workdir", lambda args, workdir: args
    )
    monkeypatch.setattr("runner.handlers._sanitized_env", lambda: {})

    worker_argv: list[str] = []

    def _worker_run(args, **kwargs):
        worker_argv[:] = args
        return BoundedProcessResult(tuple(args), 1, "", "expected", False)

    monkeypatch.setattr("runner.handler_codergen.run_bounded_process", _worker_run)
    worker = Node(
        name="worker",
        attrs={"type": "codergen", "backend": "codex", "prompt": "implement"},
    )
    _codergen(worker, Context(goal="test", workdir=tmp_path, backend="codex"))

    popen_argv: list[list[str]] = []

    class _FakePopen:
        def __init__(self, args, **kwargs):
            popen_argv.append(list(args))

    monkeypatch.setattr("runner.handler_codergen.subprocess.Popen", _FakePopen)
    shadow_ctx = Context(goal="test", workdir=tmp_path, backend="claude")
    shadow_ctx.state["_df_shadow_codex_review"] = "true"
    _start_shadow_codex_review(
        Node(name="review", attrs={"class": "review"}),
        shadow_ctx,
        "claude",
        "review",
    )

    ctx = Context(goal="test", workdir=tmp_path, backend="codex")
    gate_argv = _gate_subprocess_args("codex", "review", ctx, 300)
    assert gate_argv is not None
    controller_argv = _controller_codex_args(gate_argv)

    monkeypatch.setattr("runner.handler_dispatch.subprocess.Popen", _FakePopen)
    _launch_shadow_gate_review("gate", "review", "a" * 40, 300, ctx)
    skeptic_argv = _build_reviewer_cmd("codex", "")

    assert worker_argv[0] == resolved
    assert gate_argv[0] == resolved
    assert controller_argv[0] == resolved
    assert skeptic_argv[0] == resolved
    assert len(popen_argv) == 2
    assert all(argv[0] == resolved for argv in popen_argv)


def test_codex_availability_probe_is_static(tmp_path: Path, monkeypatch) -> None:
    from runner import codex_runtime
    from runner.handler_dispatch import _probe_backend_installed

    resolved = str(tmp_path / ".nvm/versions/node/v22.22.0/bin/codex")
    monkeypatch.setattr(codex_runtime, "resolve_codex_executable", lambda: resolved)

    def _forbidden(*args, **kwargs):
        raise AssertionError("availability probe must not start codex --version")

    monkeypatch.setattr(subprocess, "run", _forbidden)
    assert _probe_backend_installed("codex") is True


def test_codex_preflight_fails_closed_on_static_runtime_skew(monkeypatch) -> None:
    from runner import codex_runtime, preflight

    monkeypatch.setattr(preflight, "_probe", lambda name: f"/fake/{name}")

    def _skew():
        raise codex_runtime.CodexRuntimeError("cache client_version mismatch")

    monkeypatch.setattr(codex_runtime, "resolve_codex_runtime", _skew)
    result = preflight.preflight_check("codex")

    assert result["status"] == "fail"
    assert result["configured_ok"] is False
    assert "cache client_version mismatch" in result["message"]


def test_installer_and_wrapper_use_static_codex_runtime_diagnostics() -> None:
    root = Path(__file__).resolve().parents[1]
    installer = (root / "install.sh").read_text(encoding="utf-8")
    wrapper = (root / "bin" / "dark-factory").read_text(encoding="utf-8")

    assert "-m runner.codex_runtime --json" in installer
    assert "-m runner.preflight" in wrapper
    assert 'PREFLIGHT_SHADOW_CODEX="true"' in wrapper
    assert '_df_shadow_codex_review=false' in wrapper
    assert '--shadow-codex "${PREFLIGHT_SHADOW_CODEX}"' in wrapper


def test_fresh_worker_and_controller_share_aligned_cache_and_emit_artifacts(
    tmp_path: Path,
) -> None:
    home = tmp_path / "home"
    executable, _, cache = _write_runtime(home)
    executable.resolve().write_text(
        """#!/usr/bin/env python3
import json
import sys
from pathlib import Path

cache = json.loads((Path.home() / ".codex/models_cache.json").read_text())
assert cache["client_version"] == "0.146.0"
assert all(isinstance(model["display_name"], str) for model in cache["models"])
assert all("supports_reasoning_summaries" not in model for model in cache["models"])
prompt = sys.stdin.read() if "-" in sys.argv else sys.argv[-1]
if "--json" not in sys.argv:
    print("worker-ok")
    raise SystemExit(0)
response = []
for line in prompt.splitlines():
    if line.startswith(("PROMPT_ID:", "PROMPT_SHA256:", "ENVELOPE_SHA256:", "HEAD_SHA:", "TASK_SHA256:", "DIFF_SHA256:", "CHANGED_FILES_SHA256:", "EVIDENCE_MANIFEST_SHA256:")):
        response.append(line)
response.append("VERDICT: pass")
response.extend(f"C{i}: pass" for i in range(8))
response.extend(f"E{i}: pass" for i in range(15))
response.extend(["## Findings", "none", "## Commands Executed", "none", "## Evidence Checked", "fixture", "## Caveats", "none"])
print(json.dumps({"type": "item.completed", "item": {"type": "agent_message", "text": "\\n".join(response) + "\\n"}}))
""",
        encoding="utf-8",
    )
    executable.resolve().chmod(0o755)
    before_bytes = cache.read_bytes()
    before_mtime = cache.stat().st_mtime_ns
    root = Path(__file__).resolve().parents[1]
    env = {
        **os.environ,
        "HOME": str(home),
        "PYTHONPATH": str(root),
        "DISABLE_SANDBOX": "1",
        "DARK_FACTORY_HOLDOUTS": str(root / "tests/fixtures/holdout-eval"),
    }
    env.pop("DARK_FACTORY_ITERATION_STUB", None)
    env.pop("DARK_FACTORY_FAKE_LLM", None)
    workdir = tmp_path / "target"
    workdir.mkdir()

    worker_code = """
import json
import pathlib
from runner.handlers import Context, _codergen
from runner.parser import Node
ctx = Context(goal="fixture", workdir=pathlib.Path({workdir!r}), backend="codex")
node = Node(name="worker", attrs={{"type": "codergen", "backend": "codex", "prompt": "implement"}})
result = _codergen(node, ctx)
print(json.dumps({{"outcome": result.outcome, "output": result.output}}))
""".format(workdir=str(workdir))
    worker = subprocess.run(
        [sys.executable, "-c", worker_code],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert worker.returncode == 0, worker.stderr
    assert json.loads(worker.stdout)["outcome"] == "success"

    artifacts = tmp_path / "controller-artifacts"
    controller_code = """
import json
import os
import pathlib
from runner.handler_core import Context
from runner.handler_dispatch import _controller_codex_args, _gate_subprocess_args
from runner.review_controller import ReviewInputs, create_review_request, run_controller_review
workdir = pathlib.Path({workdir!r})
output_dir = pathlib.Path({artifacts!r})
request = create_review_request(ReviewInputs(
    repository="fixture/repo", workspace_path=str(workdir), base_sha="0" * 40,
    head_sha="1" * 40, tree_sha="2" * 40, task_text="task", diff_text="diff",
    changed_files=("file.py",), run_id="fixture-run",
))
ctx = Context(goal="fixture", workdir=workdir, backend="codex")
args = _gate_subprocess_args("codex", request.prompt, ctx, 30)
assert args is not None
transport = _controller_codex_args(args)
result = run_controller_review(
    request, neutral_cwd=workdir, output_dir=output_dir,
    transport_argv=tuple(transport), transport_env=os.environ, timeout=30,
)
print(json.dumps({{"verdict": result.review.verdict, "paths": result.output_paths, "argv": transport}}))
""".format(workdir=str(workdir), artifacts=str(artifacts))
    controller = subprocess.run(
        [sys.executable, "-c", controller_code],
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert controller.returncode == 0, controller.stderr
    payload = json.loads(controller.stdout)
    assert payload["verdict"] == "pass"
    assert payload["argv"][0] == str(executable)
    assert {"prompt", "envelope", "response", "transport", "receipt", "findings"} <= set(
        payload["paths"]
    )
    assert all(Path(path).is_file() for path in payload["paths"].values())
    assert cache.read_bytes() == before_bytes
    assert cache.stat().st_mtime_ns == before_mtime
