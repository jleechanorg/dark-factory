"""Pinned Codex runtime and shared-cache contract tests."""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from hashlib import sha256
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
    monkeypatch.setattr(
        codex_runtime,
        "resolve_codex_executable",
        lambda requested=None: resolved,
    )
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


def test_skeptic_codex_override_cannot_bypass_canonical_resolver(monkeypatch) -> None:
    from runner import codex_runtime
    from runner.skeptic_gate_cli import _build_reviewer_cmd

    requested: list[str | None] = []

    def _reject_override(value=None):
        requested.append(value)
        raise codex_runtime.CodexRuntimeError("not the canonical Node 22 executable")

    monkeypatch.setattr(codex_runtime, "resolve_codex_executable", _reject_override)

    with pytest.raises(codex_runtime.CodexRuntimeError, match="canonical Node 22"):
        _build_reviewer_cmd("codex", "", codex_bin="/tmp/untrusted-codex")

    assert requested == ["/tmp/untrusted-codex"]


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

    assert '-m runner.codex_runtime "${CODEX_RUNTIME_ARGS[@]}"' in installer
    assert "-m runner.preflight" in wrapper
    assert 'PREFLIGHT_SHADOW_CODEX="true"' in wrapper
    assert "__df_apply_shadow_state" in wrapper
    assert '--shadow-codex "${PREFLIGHT_SHADOW_CODEX}"' in wrapper


def _write_sync_fakes(
    home: Path,
    *,
    codex_exit: int = 0,
    codex_output: str = "CODEX_RUNTIME_READY",
    refresh_cache: bool = True,
    node_version: str = NODE_VERSION,
) -> tuple[Path, bytes]:
    _, _, cache = _write_runtime(
        home,
        package_version="0.144.5",
        cache_version="0.144.5",
    )
    before_cache = cache.read_bytes()
    desired_cache = json.dumps(
        {
            "fetched_at": "2026-08-03T00:00:00Z",
            "etag": None,
            "client_version": PINNED_VERSION,
            "models": [_release_model()],
        },
        separators=(",", ":"),
    ).encode()
    (home / "desired-cache.json").write_bytes(desired_cache)
    node_root = home / ".nvm" / "versions" / "node" / NODE_VERSION
    package_json = node_root / "lib/node_modules/@openai/codex/package.json"
    events = home / "sync-events.jsonl"
    node = node_root / "bin/node"
    node.write_text(
        f"""#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

events = Path({str(events)!r})
if sys.argv[1:] == ["--version"]:
    with events.open("a") as stream:
        stream.write(json.dumps({{"tool": "node_version", "argv": sys.argv[1:]}}) + "\\n")
    print({node_version!r})
    raise SystemExit(0)
os.execv(sys.executable, [sys.executable, *sys.argv[1:]])
""",
        encoding="utf-8",
    )
    node.chmod(0o755)
    npm_cli = node_root / "lib/node_modules/npm/bin/npm-cli.js"
    npm_cli.parent.mkdir(parents=True, exist_ok=True)
    npm_cli.write_text(
        f"""#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

home = Path(os.environ["HOME"])
backups = list((home / ".dark-factory/backups/codex-runtime").glob("models_cache.*.json"))
with (home / "sync-events.jsonl").open("a") as stream:
    stream.write(json.dumps({{"tool": "npm", "argv": sys.argv[1:], "backup_count": len(backups)}}) + "\\n")
Path({str(package_json)!r}).write_text(json.dumps({{"name": "@openai/codex", "version": "{PINNED_VERSION}", "bin": {{"codex": "bin/codex.js"}}}}))
""",
        encoding="utf-8",
    )
    npm = node_root / "bin/npm"
    npm.write_text("#!/usr/bin/env node\nraise SystemExit(99)\n", encoding="utf-8")
    npm.chmod(0o755)
    codex = node_root / "lib/node_modules/@openai/codex/bin/codex.js"
    codex.write_text(
        f"""#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

home = Path(os.environ["HOME"])
with (home / "sync-events.jsonl").open("a") as stream:
    stream.write(json.dumps({{"tool": "codex", "argv": sys.argv[1:], "cwd": os.getcwd()}}) + "\\n")
if {refresh_cache!r}:
    (home / ".codex/models_cache.json").write_bytes((home / "desired-cache.json").read_bytes())
print({codex_output!r})
raise SystemExit({codex_exit})
""",
        encoding="utf-8",
    )
    codex.chmod(0o755)
    return cache, before_cache


def test_default_runtime_cli_is_read_only_and_sync_is_explicit(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    from runner import codex_runtime

    home = tmp_path / "home"
    _, package_json, cache = _write_runtime(home)
    package_before = package_json.read_bytes()
    cache_before = cache.read_bytes()
    root = Path(__file__).resolve().parents[1]
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setattr(codex_runtime, "_default_competing_package_paths", lambda: ())

    returncode = codex_runtime.main(["--json"])

    assert returncode == 0
    assert json.loads(capsys.readouterr().out)["status"] == "pass"
    assert package_json.read_bytes() == package_before
    assert cache.read_bytes() == cache_before
    installer = (root / "install.sh").read_text(encoding="utf-8")
    assert "--sync-codex-runtime" in installer
    assert 'CODEX_RUNTIME_ARGS+=(--sync)' in installer


def test_sync_uses_exact_node22_tools_backs_up_first_and_validates(
    tmp_path: Path, monkeypatch
) -> None:
    from runner.codex_runtime import sync_codex_runtime

    home = tmp_path / "home"
    cache, before_cache = _write_sync_fakes(home)
    checkout_tmp = tmp_path / "checkout" / "tmp"
    (checkout_tmp.parent / ".git").mkdir(parents=True)
    checkout_tmp.mkdir()
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("TMPDIR", str(checkout_tmp))
    monkeypatch.setenv("OPENAI_API_KEY", "must-not-appear-in-evidence")

    evidence = sync_codex_runtime(home=home, competing_package_paths=())

    node_root = home / ".nvm" / "versions" / "node" / NODE_VERSION
    events = [json.loads(line) for line in (home / "sync-events.jsonl").read_text().splitlines()]
    assert events[0] == {"tool": "node_version", "argv": ["--version"]}
    assert events[1] == {
        "tool": "npm",
        "argv": [
            "install",
            "--global",
            "--prefix",
            str(node_root),
            f"@openai/codex@{PINNED_VERSION}",
        ],
        "backup_count": 1,
    }
    assert events[2]["tool"] == "codex"
    assert events[2]["argv"] == [
        "exec",
        "--sandbox",
        "read-only",
        "--skip-git-repo-check",
        "CODEX_RUNTIME_READY",
    ]
    startup_cwd = Path(events[2]["cwd"])
    assert startup_cwd.is_relative_to(home / ".dark-factory/tmp/codex-runtime")
    assert not startup_cwd.is_relative_to(checkout_tmp.parent)
    assert evidence["temporary_workdir"] == str(startup_cwd)
    assert stat.S_IMODE(
        (home / ".dark-factory/tmp/codex-runtime").stat().st_mode
    ) == 0o700
    assert "login" not in events[2]["argv"]
    backup = Path(evidence["backup_path"])
    assert backup.read_bytes() == before_cache
    assert cache.read_bytes() == (home / "desired-cache.json").read_bytes()
    assert evidence["status"] == "pass"
    assert evidence["phase"] == "complete"
    assert evidence["package_version"] == {"before": "0.144.5", "after": PINNED_VERSION}
    assert evidence["cache"]["before"] == {
        "sha256": sha256(before_cache).hexdigest(),
        "client_version": "0.144.5",
    }
    assert evidence["cache"]["after"] == {
        "sha256": sha256(cache.read_bytes()).hexdigest(),
        "client_version": PINNED_VERSION,
    }
    assert evidence["subprocesses"]["node_version"]["argv"] == [
        str(node_root / "bin/node"),
        "--version",
    ]
    assert evidence["subprocesses"]["npm_install"]["argv"][:2] == [
        str(node_root / "bin/node"),
        str(node_root / "lib/node_modules/npm/bin/npm-cli.js"),
    ]
    assert evidence["subprocesses"]["codex_startup"]["argv"][0] == str(node_root / "bin/codex")
    assert evidence["subprocesses"]["codex_startup"]["timeout_seconds"] == 120
    assert evidence["readiness_token"] == "CODEX_RUNTIME_READY"
    assert evidence["resolver"]["status"] == "pass"
    assert evidence["resolver"]["version"] == PINNED_VERSION
    assert "must-not-appear-in-evidence" not in json.dumps(evidence)


def test_sync_never_falls_back_to_ambient_node(tmp_path: Path, monkeypatch) -> None:
    from runner.codex_runtime import CodexRuntimeSyncError, sync_codex_runtime

    home = tmp_path / "home"
    _write_sync_fakes(home)
    (home / ".nvm/versions/node" / NODE_VERSION / "bin/node").unlink()
    ambient_bin = tmp_path / "ambient-bin"
    ambient_bin.mkdir()
    ambient_marker = tmp_path / "ambient-node-ran"
    ambient_node = ambient_bin / "node"
    ambient_node.write_text(
        f"#!/bin/sh\ntouch {ambient_marker}\nexit 0\n",
        encoding="utf-8",
    )
    ambient_node.chmod(0o755)
    monkeypatch.setenv("PATH", f"{ambient_bin}:{os.environ['PATH']}")

    with pytest.raises(CodexRuntimeSyncError) as caught:
        sync_codex_runtime(home=home, competing_package_paths=())

    assert caught.value.evidence["phase"] == "preflight"
    assert "canonical Node" in caught.value.evidence["error"]
    assert not ambient_marker.exists()
    assert caught.value.evidence["backup_path"] is None


def test_sync_rejects_wrong_canonical_node_version_before_backup(
    tmp_path: Path, monkeypatch
) -> None:
    from runner.codex_runtime import CodexRuntimeSyncError, sync_codex_runtime

    home = tmp_path / "home"
    _write_sync_fakes(home, node_version="v24.0.0")

    with pytest.raises(CodexRuntimeSyncError) as caught:
        sync_codex_runtime(home=home, competing_package_paths=())

    assert caught.value.evidence["phase"] == "preflight"
    assert "Node version mismatch" in caught.value.evidence["error"]
    assert caught.value.evidence["backup_path"] is None
    assert not (home / ".dark-factory/backups/codex-runtime").exists()


def test_sync_rejects_runtime_tempdir_inside_git_worktree(
    tmp_path: Path, monkeypatch
) -> None:
    from runner.codex_runtime import CodexRuntimeSyncError, sync_codex_runtime

    checkout = tmp_path / "checkout"
    (checkout / ".git").mkdir(parents=True)
    home = checkout / "home"
    _, before_cache = _write_sync_fakes(home)

    with pytest.raises(CodexRuntimeSyncError) as caught:
        sync_codex_runtime(home=home, competing_package_paths=())

    evidence = caught.value.evidence
    assert evidence["phase"] == "codex_tempdir"
    assert "Git worktree" in evidence["error"]
    assert Path(evidence["backup_path"]).read_bytes() == before_cache
    events = [json.loads(line) for line in (home / "sync-events.jsonl").read_text().splitlines()]
    assert [event["tool"] for event in events] == ["node_version", "npm"]


def test_sync_structures_runtime_tempdir_creation_failure(
    tmp_path: Path, monkeypatch
) -> None:
    from runner import codex_runtime

    home = tmp_path / "home"
    _, before_cache = _write_sync_fakes(home)

    def _fail_create(*args, **kwargs):
        raise OSError("fixture create denied")

    monkeypatch.setattr(codex_runtime, "_create_runtime_tempdir", _fail_create)

    with pytest.raises(codex_runtime.CodexRuntimeSyncError) as caught:
        codex_runtime.sync_codex_runtime(home=home, competing_package_paths=())

    evidence = caught.value.evidence
    assert evidence["phase"] == "codex_tempdir"
    assert "fixture create denied" in evidence["error"]
    assert Path(evidence["backup_path"]).read_bytes() == before_cache
    assert evidence["package_version"]["after"] == PINNED_VERSION


@pytest.mark.parametrize("codex_exit", [0, 7])
def test_sync_structures_cleanup_failure_without_masking_primary_error(
    tmp_path: Path, monkeypatch, codex_exit: int
) -> None:
    from runner import codex_runtime

    home = tmp_path / "home"
    _, before_cache = _write_sync_fakes(home, codex_exit=codex_exit)

    def _fail_cleanup(path):
        raise OSError("fixture cleanup denied")

    monkeypatch.setattr(codex_runtime, "_cleanup_runtime_tempdir", _fail_cleanup)

    with pytest.raises(codex_runtime.CodexRuntimeSyncError) as caught:
        codex_runtime.sync_codex_runtime(home=home, competing_package_paths=())

    evidence = caught.value.evidence
    expected_phase = "codex_tempdir_cleanup" if codex_exit == 0 else "codex_startup"
    assert evidence["phase"] == expected_phase
    assert "fixture cleanup denied" in (
        evidence["error"] if codex_exit == 0 else evidence["cleanup_error"]
    )
    assert Path(evidence["backup_path"]).read_bytes() == before_cache


def test_backup_is_private_unique_and_retries_collision(
    tmp_path: Path, monkeypatch
) -> None:
    from runner import codex_runtime

    home = tmp_path / "home"
    _, _, cache = _write_runtime(home)
    backup_dir = home / ".dark-factory/backups/codex-runtime"
    backup_dir.mkdir(parents=True, mode=0o700)
    collision = backup_dir / "models_cache.collision.json"
    collision.write_bytes(b"keep-me")
    unique = backup_dir / "models_cache.unique.json"
    candidates = iter((collision, unique))
    monkeypatch.setattr(codex_runtime, "_backup_candidate", lambda _: next(candidates))

    backup = codex_runtime._backup_cache(cache, home)

    assert backup == unique
    assert collision.read_bytes() == b"keep-me"
    assert backup.read_bytes() == cache.read_bytes()
    assert stat.S_IMODE(backup.stat().st_mode) == 0o600
    assert stat.S_IMODE(backup_dir.stat().st_mode) == 0o700


@pytest.mark.parametrize("unsafe_kind", ["permissions", "symlink"])
def test_sync_rejects_unsafe_backup_directory(
    tmp_path: Path, monkeypatch, unsafe_kind: str
) -> None:
    from runner.codex_runtime import CodexRuntimeSyncError, sync_codex_runtime

    home = tmp_path / "home"
    _write_sync_fakes(home)
    backup_dir = home / ".dark-factory/backups/codex-runtime"
    if unsafe_kind == "permissions":
        backup_dir.mkdir(parents=True)
        backup_dir.chmod(0o755)
    else:
        backup_dir.parent.mkdir(parents=True)
        target = tmp_path / "redirected-backups"
        target.mkdir()
        backup_dir.symlink_to(target, target_is_directory=True)

    with pytest.raises(CodexRuntimeSyncError) as caught:
        sync_codex_runtime(home=home, competing_package_paths=())

    assert caught.value.evidence["phase"] == "backup"
    assert unsafe_kind in caught.value.evidence["error"].lower() or "private" in caught.value.evidence["error"].lower()
    events = [json.loads(line) for line in (home / "sync-events.jsonl").read_text().splitlines()]
    assert [event["tool"] for event in events] == ["node_version"]


@pytest.mark.parametrize(
    ("codex_exit", "codex_output", "phase"),
    [
        (7, "CODEX_RUNTIME_READY", "codex_startup"),
        (0, "NOT_READY", "readiness"),
    ],
)
def test_sync_fails_closed_without_rollback_on_codex_error(
    tmp_path: Path,
    monkeypatch,
    codex_exit: int,
    codex_output: str,
    phase: str,
) -> None:
    from runner.codex_runtime import CodexRuntimeSyncError, sync_codex_runtime

    home = tmp_path / "home"
    cache, before_cache = _write_sync_fakes(
        home,
        codex_exit=codex_exit,
        codex_output=codex_output,
    )
    monkeypatch.setenv("HOME", str(home))

    with pytest.raises(CodexRuntimeSyncError) as caught:
        sync_codex_runtime(home=home, competing_package_paths=())

    evidence = caught.value.evidence
    assert evidence["status"] == "fail"
    assert evidence["phase"] == phase
    assert Path(evidence["backup_path"]).read_bytes() == before_cache
    assert cache.read_bytes() == (home / "desired-cache.json").read_bytes()
    assert evidence["subprocesses"]["codex_startup"]["exit_code"] == codex_exit
    assert evidence["subprocesses"]["codex_startup"]["timed_out"] is False


def test_sync_reports_timeout_and_preserves_backup(tmp_path: Path, monkeypatch) -> None:
    from runner import codex_runtime
    from runner.subprocess_control import BoundedProcessResult

    home = tmp_path / "home"
    _, _, cache = _write_runtime(home)
    before_cache = cache.read_bytes()
    node_root = home / ".nvm" / "versions" / "node" / NODE_VERSION
    node = node_root / "bin/node"
    node.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    node.chmod(0o755)
    npm_cli = node_root / "lib/node_modules/npm/bin/npm-cli.js"
    npm_cli.parent.mkdir(parents=True)
    npm_cli.write_text("fixture", encoding="utf-8")
    npm = node_root / "bin/npm"
    npm.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    npm.chmod(0o755)
    calls: list[tuple[list[str], float]] = []

    def _bounded(args, *, timeout, **kwargs):
        calls.append((list(args), timeout))
        if list(args) == [str(node), "--version"]:
            return BoundedProcessResult(tuple(args), 0, f"{NODE_VERSION}\n", "", False)
        if list(args)[:2] == [str(node), str(npm_cli)]:
            return BoundedProcessResult(tuple(args), 0, "", "", False)
        return BoundedProcessResult(tuple(args), -1, "", "", True)

    monkeypatch.setattr(codex_runtime, "run_bounded_process", _bounded)

    with pytest.raises(codex_runtime.CodexRuntimeSyncError) as caught:
        codex_runtime.sync_codex_runtime(home=home, competing_package_paths=())

    evidence = caught.value.evidence
    assert evidence["phase"] == "codex_startup"
    assert evidence["subprocesses"]["codex_startup"]["timed_out"] is True
    assert calls[2][1] == 120
    assert Path(evidence["backup_path"]).read_bytes() == before_cache
    assert cache.read_bytes() == before_cache


def test_sync_fails_final_static_validation_without_editing_stale_cache(
    tmp_path: Path, monkeypatch
) -> None:
    from runner.codex_runtime import CodexRuntimeSyncError, sync_codex_runtime

    home = tmp_path / "home"
    cache, before_cache = _write_sync_fakes(home, refresh_cache=False)
    monkeypatch.setenv("HOME", str(home))

    with pytest.raises(CodexRuntimeSyncError) as caught:
        sync_codex_runtime(home=home, competing_package_paths=())

    evidence = caught.value.evidence
    assert evidence["phase"] == "final_validation"
    assert "cache client_version mismatch" in evidence["error"]
    assert Path(evidence["backup_path"]).read_bytes() == before_cache
    assert cache.read_bytes() == before_cache


def test_installer_sync_flag_runs_fake_runtime_end_to_end(tmp_path: Path) -> None:
    home = tmp_path / "home"
    _write_sync_fakes(home)
    fake_bin = tmp_path / "fake-bin"
    fake_bin.mkdir()
    ambient_marker = tmp_path / "ambient-node-ran"
    scripts = {
        "uv": '#!/bin/sh\nif [ "$1" = "--version" ]; then echo "uv fake"; fi\nexit 0\n',
        "git-lfs": '#!/bin/sh\necho "git-lfs/fake"\nexit 0\n',
        "node": f"#!/bin/sh\ntouch {ambient_marker}\nexit 99\n",
    }
    for name, source in scripts.items():
        executable = fake_bin / name
        executable.write_text(source, encoding="utf-8")
        executable.chmod(0o755)
    root = Path(__file__).resolve().parents[1]

    result = subprocess.run(
        [
            str(root / "install.sh"),
            "--sync-codex-runtime",
            "--no-link",
            "--no-cmds",
            "--no-smoke",
        ],
        cwd=root,
        env={
            **os.environ,
            "HOME": str(home),
            "PATH": f"{fake_bin}:{os.environ['PATH']}",
        },
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    payloads = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
    assert payloads[-1]["status"] == "pass"
    assert payloads[-1]["phase"] == "complete"
    assert not ambient_marker.exists()


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
