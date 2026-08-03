"""Static, fail-closed contract for the one supported Codex CLI runtime."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import stat
import tempfile
import uuid
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable

from runner.subprocess_control import BoundedProcessResult, run_bounded_process


PINNED_CODEX_VERSION = "0.146.0"
PINNED_NODE_VERSION = "v22.22.0"
CODEX_RUNTIME_READY = "CODEX_RUNTIME_READY"
SYNC_TIMEOUT_SECONDS = 120
_PACKAGE_RELATIVE = Path("lib/node_modules/@openai/codex")
_EXECUTABLE_RELATIVE = Path("bin/codex")
_REQUIRED_MODEL_FIELDS = {
    "slug": str,
    "display_name": str,
    "description": (str, type(None)),
    "supported_reasoning_levels": list,
    "shell_type": str,
    "visibility": str,
    "supported_in_api": bool,
    "priority": int,
    "additional_speed_tiers": list,
    "service_tiers": list,
    "availability_nux": (dict, type(None)),
    "upgrade": (dict, type(None)),
    "base_instructions": str,
    "include_skills_usage_instructions": bool,
    "default_reasoning_summary": str,
    "support_verbosity": bool,
    "default_verbosity": (str, type(None)),
    "apply_patch_tool_type": (str, type(None)),
    "web_search_tool_type": str,
    "truncation_policy": dict,
    "supports_parallel_tool_calls": bool,
    "supports_image_detail_original": bool,
    "effective_context_window_percent": int,
    "experimental_supported_tools": list,
    "input_modalities": list,
    "supports_search_tool": bool,
    "use_responses_lite": bool,
}
_OPTIONAL_MODEL_FIELDS = {
    "default_reasoning_level": (str, type(None)),
    "default_service_tier": (str, type(None)),
    "model_messages": (dict, type(None)),
    "supports_reasoning_summary_parameter": bool,
    "context_window": (int, type(None)),
    "max_context_window": (int, type(None)),
    "auto_compact_token_limit": (int, type(None)),
    "comp_hash": (str, type(None)),
    "auto_review_model_override": (str, type(None)),
    "tool_mode": (str, type(None)),
    "multi_agent_version": (str, type(None)),
}


class CodexRuntimeError(RuntimeError):
    """The installed Codex executable, metadata, or shared cache is unsafe."""


class CodexRuntimeSyncError(CodexRuntimeError):
    """An opt-in runtime deployment failed without automatic rollback."""

    def __init__(self, phase: str, message: str, evidence: dict[str, object]) -> None:
        super().__init__(message)
        evidence.update(status="fail", phase=phase, error=message)
        self.evidence = evidence


class _CodexRuntimeTempdirError(CodexRuntimeError):
    """Preserve a tempdir primary error plus a secondary cleanup error."""

    def __init__(self, primary_error: Exception, cleanup_error: OSError) -> None:
        super().__init__(str(primary_error))
        self.cleanup_error = str(cleanup_error)


@dataclass(frozen=True)
class CodexRuntime:
    executable: Path
    package_json: Path
    version: str
    cache_path: Path


def _read_json_object(path: Path, label: str) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CodexRuntimeError(f"{label} is unreadable or invalid JSON at {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise CodexRuntimeError(f"{label} must be a JSON object at {path}")
    return value


def _package_version(path: Path, label: str) -> str:
    package = _read_json_object(path, label)
    if package.get("name") != "@openai/codex":
        raise CodexRuntimeError(f"{label} has unexpected package name at {path}")
    version = package.get("version")
    if not isinstance(version, str) or not version:
        raise CodexRuntimeError(f"{label} has no static version at {path}")
    return version


def _validate_cache(cache_path: Path) -> None:
    if not cache_path.exists():
        return
    cache = _read_json_object(cache_path, "Codex models cache")
    client_version = cache.get("client_version")
    if client_version != PINNED_CODEX_VERSION:
        raise CodexRuntimeError(
            "Codex models cache client_version mismatch: "
            f"expected {PINNED_CODEX_VERSION}, got {client_version!r} at {cache_path}"
        )
    fetched_at = cache.get("fetched_at")
    if not isinstance(fetched_at, str):
        raise CodexRuntimeError(
            f"Codex models cache requires fetched_at as RFC3339 text at {cache_path}"
        )
    try:
        parsed_fetched_at = datetime.fromisoformat(fetched_at.replace("Z", "+00:00"))
    except ValueError as exc:
        raise CodexRuntimeError(
            f"Codex models cache fetched_at is not RFC3339 at {cache_path}"
        ) from exc
    if parsed_fetched_at.tzinfo is None:
        raise CodexRuntimeError(
            f"Codex models cache fetched_at must include a timezone at {cache_path}"
        )
    models = cache.get("models")
    if not isinstance(models, list) or not models:
        raise CodexRuntimeError(f"Codex models cache has incompatible models schema at {cache_path}")
    for index, model in enumerate(models):
        if not isinstance(model, dict):
            raise CodexRuntimeError(
                f"Codex models cache model {index} is not an object at {cache_path}"
            )
        for field, expected_type in _REQUIRED_MODEL_FIELDS.items():
            if field not in model or not _matches_json_type(model[field], expected_type):
                raise CodexRuntimeError(
                    f"Codex models cache model {index} requires {field} "
                    f"as {_type_name(expected_type)} at {cache_path}"
                )
        for field, expected_type in _OPTIONAL_MODEL_FIELDS.items():
            if field in model and not _matches_json_type(model[field], expected_type):
                raise CodexRuntimeError(
                    f"Codex models cache model {index} requires optional {field} "
                    f"as {_type_name(expected_type)} when present at {cache_path}"
                )


def _matches_json_type(value: object, expected_type: type | tuple[type, ...]) -> bool:
    allowed = expected_type if isinstance(expected_type, tuple) else (expected_type,)
    if int in allowed and bool not in allowed and isinstance(value, bool):
        return False
    return isinstance(value, allowed)


def _type_name(expected_type: type | tuple[type, ...]) -> str:
    allowed = expected_type if isinstance(expected_type, tuple) else (expected_type,)
    return " or ".join(item.__name__ for item in allowed)


def _default_competing_package_paths() -> tuple[Path, ...]:
    """Known global npm roots that share the user's default Codex cache."""
    return (
        Path("/opt/homebrew/lib/node_modules/@openai/codex/package.json"),
        Path("/usr/local/lib/node_modules/@openai/codex/package.json"),
    )


def resolve_codex_runtime(
    *,
    home: Path | None = None,
    cache_path: Path | None = None,
    competing_package_paths: Iterable[Path] | None = None,
) -> CodexRuntime:
    """Resolve and statically validate the pinned Node 22 Codex runtime."""
    home = (home or Path.home()).expanduser().resolve()
    node_root = home / ".nvm" / "versions" / "node" / PINNED_NODE_VERSION
    executable = node_root / _EXECUTABLE_RELATIVE
    if not executable.is_file() or not os.access(executable, os.X_OK):
        raise CodexRuntimeError(
            f"canonical Codex executable is missing or not executable at {executable}"
        )

    package_root = node_root / _PACKAGE_RELATIVE
    package_json = package_root / "package.json"
    try:
        resolved_executable = executable.resolve(strict=True)
        expected_executable = (package_root / "bin" / "codex.js").resolve(strict=True)
    except OSError as exc:
        raise CodexRuntimeError(f"canonical Codex executable cannot be resolved: {exc}") from exc
    if resolved_executable != expected_executable:
        raise CodexRuntimeError(
            f"canonical Codex executable must resolve inside {package_root}, got {resolved_executable}"
        )

    version = _package_version(package_json, "canonical Codex package metadata")
    if version != PINNED_CODEX_VERSION:
        raise CodexRuntimeError(
            f"installed Codex version mismatch: expected {PINNED_CODEX_VERSION}, "
            f"got {version!r} at {package_json}"
        )

    competitors = (
        tuple(competing_package_paths)
        if competing_package_paths is not None
        else _default_competing_package_paths()
    )
    for competitor in competitors:
        competitor = Path(competitor).expanduser()
        if competitor == package_json or not competitor.exists():
            continue
        competitor_version = _package_version(competitor, "competing Codex package metadata")
        if competitor_version != PINNED_CODEX_VERSION:
            raise CodexRuntimeError(
                f"competing Codex version mismatch: expected {PINNED_CODEX_VERSION}, "
                f"got {competitor_version!r} at {competitor}"
            )

    resolved_cache = (
        Path(cache_path).expanduser()
        if cache_path is not None
        else home / ".codex" / "models_cache.json"
    )
    _validate_cache(resolved_cache)
    return CodexRuntime(
        executable=executable,
        package_json=package_json,
        version=version,
        cache_path=resolved_cache,
    )


def resolve_codex_executable(requested: str | Path | None = None) -> str:
    """Return the only executable allowed for a Dark Factory Codex process.

    A caller-provided pin is an assertion, not an override: it must resolve to
    the already validated canonical Node 22 executable and cannot weaken the
    package/cache contract.
    """
    runtime = resolve_codex_runtime()
    if requested:
        requested_path = Path(requested)
        if not requested_path.is_absolute():
            raise CodexRuntimeError(
                f"explicit Codex executable must be absolute: {requested}"
            )
        try:
            requested_target = requested_path.resolve(strict=True)
            canonical_target = runtime.executable.resolve(strict=True)
        except OSError as exc:
            raise CodexRuntimeError(
                f"explicit Codex executable cannot be resolved: {requested}: {exc}"
            ) from exc
        if requested_target != canonical_target:
            raise CodexRuntimeError(
                "explicit Codex executable is not the canonical Node 22 runtime: "
                f"requested {requested_path}, expected {runtime.executable}"
            )
    return str(runtime.executable)


def _package_version_evidence(package_json: Path) -> str | None:
    if not package_json.exists():
        return None
    try:
        return _package_version(package_json, "canonical Codex package metadata")
    except CodexRuntimeError:
        return None


def _cache_evidence(cache_path: Path) -> dict[str, str | None]:
    if not cache_path.exists():
        return {"sha256": None, "client_version": None}
    try:
        cache_bytes = cache_path.read_bytes()
    except OSError:
        return {"sha256": None, "client_version": None}
    client_version = None
    try:
        payload = json.loads(cache_bytes)
        if isinstance(payload, dict) and isinstance(payload.get("client_version"), str):
            client_version = payload["client_version"]
    except (UnicodeError, json.JSONDecodeError):
        pass
    return {
        "sha256": hashlib.sha256(cache_bytes).hexdigest(),
        "client_version": client_version,
    }


def _process_evidence(
    result: BoundedProcessResult, *, timeout_seconds: int
) -> dict[str, object]:
    return {
        "argv": list(result.args),
        "exit_code": result.returncode,
        "timed_out": result.timed_out,
        "timeout_seconds": timeout_seconds,
    }


def _ensure_private_directory(path: Path, home: Path) -> None:
    """Create a private directory without accepting redirects or foreign owners."""
    try:
        relative = path.relative_to(home)
    except ValueError as exc:
        raise CodexRuntimeError(f"private directory escapes HOME: {path}") from exc
    current = home
    for part in relative.parts:
        current /= part
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            current.mkdir(mode=0o700)
            current.chmod(0o700)
            metadata = current.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            raise CodexRuntimeError(f"private directory path contains symlink: {current}")
        if not stat.S_ISDIR(metadata.st_mode):
            raise CodexRuntimeError(f"private directory path is not a directory: {current}")
        if metadata.st_uid != os.getuid():
            raise CodexRuntimeError(f"private directory path has unsafe owner: {current}")
    if stat.S_IMODE(path.lstat().st_mode) != 0o700:
        raise CodexRuntimeError(f"private directory permissions must be 0700: {path}")


def _backup_candidate(backup_dir: Path) -> Path:
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    return backup_dir / f"models_cache.{timestamp}.{uuid.uuid4().hex}.json"


def _backup_cache(cache_path: Path, home: Path) -> Path | None:
    try:
        cache_metadata = cache_path.lstat()
    except FileNotFoundError:
        return None
    if stat.S_ISLNK(cache_metadata.st_mode) or not stat.S_ISREG(cache_metadata.st_mode):
        raise CodexRuntimeError(f"Codex models cache must be a regular non-symlink file: {cache_path}")
    backup_dir = home / ".dark-factory" / "backups" / "codex-runtime"
    _ensure_private_directory(backup_dir, home)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    for _ in range(10):
        backup = _backup_candidate(backup_dir)
        try:
            descriptor = os.open(backup, flags, 0o600)
        except FileExistsError:
            continue
        try:
            os.fchmod(descriptor, 0o600)
            with cache_path.open("rb") as source, os.fdopen(descriptor, "wb") as destination:
                descriptor = -1
                while chunk := source.read(1024 * 1024):
                    destination.write(chunk)
        finally:
            if descriptor >= 0:
                os.close(descriptor)
        return backup
    raise CodexRuntimeError("could not allocate a unique Codex cache backup after 10 attempts")


def _assert_outside_git_worktree(path: Path) -> None:
    for candidate in (path, *path.parents):
        if os.path.lexists(candidate / ".git"):
            raise CodexRuntimeError(f"Codex runtime temporary cwd is inside a Git worktree: {path}")


def _create_runtime_tempdir(home: Path) -> Path:
    temp_root = home / ".dark-factory" / "tmp" / "codex-runtime"
    _ensure_private_directory(temp_root, home)
    _assert_outside_git_worktree(temp_root)
    temp_workdir = Path(tempfile.mkdtemp(prefix="run-", dir=temp_root))
    try:
        temp_workdir.chmod(0o700)
        _assert_outside_git_worktree(temp_workdir)
    except (OSError, CodexRuntimeError) as primary_error:
        try:
            _cleanup_runtime_tempdir(temp_workdir)
        except OSError as cleanup_error:
            raise _CodexRuntimeTempdirError(primary_error, cleanup_error) from primary_error
        raise
    return temp_workdir


def _cleanup_runtime_tempdir(path: Path) -> None:
    shutil.rmtree(path)


def _runtime_payload(runtime: CodexRuntime) -> dict[str, str]:
    return {
        "status": "pass",
        **{key: str(value) for key, value in asdict(runtime).items()},
    }


def sync_codex_runtime(
    *,
    home: Path | None = None,
    competing_package_paths: Iterable[Path] | None = None,
) -> dict[str, object]:
    """Opt in to the pinned npm install, Codex-owned cache refresh, and validation."""
    home = (home or Path.home()).expanduser().resolve()
    node_root = home / ".nvm" / "versions" / "node" / PINNED_NODE_VERSION
    node_bin = node_root / "bin"
    node = node_bin / "node"
    npm_cli = node_root / "lib" / "node_modules" / "npm" / "bin" / "npm-cli.js"
    executable = node_bin / "codex"
    package_json = node_root / _PACKAGE_RELATIVE / "package.json"
    cache_path = home / ".codex" / "models_cache.json"
    competitors = (
        tuple(competing_package_paths)
        if competing_package_paths is not None
        else _default_competing_package_paths()
    )
    evidence: dict[str, object] = {
        "status": "pending",
        "phase": "preflight",
        "backup_path": None,
        "package_version": {
            "before": _package_version_evidence(package_json),
            "after": None,
        },
        "cache": {
            "path": str(cache_path),
            "before": _cache_evidence(cache_path),
            "after": None,
        },
        "subprocesses": {},
        "readiness_token": None,
        "resolver": None,
        "temporary_workdir": None,
    }

    def fail(phase: str, message: str) -> None:
        evidence["package_version"]["after"] = _package_version_evidence(package_json)  # type: ignore[index]
        evidence["cache"]["after"] = _cache_evidence(cache_path)  # type: ignore[index]
        raise CodexRuntimeSyncError(phase, message, evidence)

    if not node.is_file() or not os.access(node, os.X_OK):
        fail("preflight", f"canonical Node is missing or not executable at {node}")
    if not npm_cli.is_file():
        fail("preflight", f"canonical Node npm CLI is missing at {npm_cli}")

    env = dict(os.environ)
    env["HOME"] = str(home)
    env["PATH"] = f"{node_bin}{os.pathsep}{env.get('PATH', '')}"
    node_argv = [str(node), "--version"]
    try:
        node_result = run_bounded_process(
            node_argv,
            timeout=SYNC_TIMEOUT_SECONDS,
            env=env,
        )
    except OSError as exc:
        evidence["subprocesses"]["node_version"] = {  # type: ignore[index]
            "argv": node_argv,
            "exit_code": None,
            "timed_out": False,
            "timeout_seconds": SYNC_TIMEOUT_SECONDS,
        }
        fail("preflight", f"could not start canonical Node: {exc}")
    evidence["subprocesses"]["node_version"] = _process_evidence(  # type: ignore[index]
        node_result,
        timeout_seconds=SYNC_TIMEOUT_SECONDS,
    )
    if node_result.timed_out:
        fail("preflight", "canonical Node version check timed out")
    if node_result.returncode != 0:
        fail("preflight", f"canonical Node version check exited {node_result.returncode}")
    if node_result.stdout.strip() != PINNED_NODE_VERSION:
        fail(
            "preflight",
            f"canonical Node version mismatch: expected {PINNED_NODE_VERSION}, "
            f"got {node_result.stdout.strip()!r}",
        )

    try:
        backup = _backup_cache(cache_path, home)
    except (OSError, CodexRuntimeError) as exc:
        fail("backup", f"could not back up Codex models cache before mutation: {exc}")
    evidence["backup_path"] = str(backup) if backup is not None else None

    npm_argv = [
        str(node),
        str(npm_cli),
        "install",
        "--global",
        "--prefix",
        str(node_root),
        f"@openai/codex@{PINNED_CODEX_VERSION}",
    ]
    try:
        npm_result = run_bounded_process(
            npm_argv,
            timeout=SYNC_TIMEOUT_SECONDS,
            env=env,
        )
    except OSError as exc:
        evidence["subprocesses"]["npm_install"] = {  # type: ignore[index]
            "argv": npm_argv,
            "exit_code": None,
            "timed_out": False,
            "timeout_seconds": SYNC_TIMEOUT_SECONDS,
        }
        fail("npm_install", f"could not start pinned Node 22 npm: {exc}")
    evidence["subprocesses"]["npm_install"] = _process_evidence(  # type: ignore[index]
        npm_result,
        timeout_seconds=SYNC_TIMEOUT_SECONDS,
    )
    if npm_result.timed_out:
        fail("npm_install", "pinned Codex npm install timed out")
    if npm_result.returncode != 0:
        fail("npm_install", f"pinned Codex npm install exited {npm_result.returncode}")
    if _package_version_evidence(package_json) != PINNED_CODEX_VERSION:
        fail("npm_validation", "npm install did not produce the pinned Codex package version")
    if not executable.is_file() or not os.access(executable, os.X_OK):
        fail("npm_validation", f"npm install did not produce executable {executable}")

    codex_argv = [
        str(executable),
        "exec",
        "--sandbox",
        "read-only",
        "--skip-git-repo-check",
        CODEX_RUNTIME_READY,
    ]
    try:
        temp_workdir = _create_runtime_tempdir(home)
    except (OSError, CodexRuntimeError) as exc:
        if isinstance(exc, _CodexRuntimeTempdirError):
            evidence["cleanup_error"] = exc.cleanup_error
        fail("codex_tempdir", f"could not create safe Codex runtime temporary cwd: {exc}")
    evidence["temporary_workdir"] = str(temp_workdir)
    primary_error: CodexRuntimeSyncError | None = None
    try:
        try:
            codex_result = run_bounded_process(
                codex_argv,
                cwd=temp_workdir,
                timeout=SYNC_TIMEOUT_SECONDS,
                env=env,
            )
        except OSError as exc:
            evidence["subprocesses"]["codex_startup"] = {  # type: ignore[index]
                "argv": codex_argv,
                "exit_code": None,
                "timed_out": False,
                "timeout_seconds": SYNC_TIMEOUT_SECONDS,
            }
            fail("codex_startup", f"could not start canonical Codex: {exc}")
        evidence["subprocesses"]["codex_startup"] = _process_evidence(  # type: ignore[index]
            codex_result,
            timeout_seconds=SYNC_TIMEOUT_SECONDS,
        )
        if codex_result.timed_out:
            fail("codex_startup", "canonical Codex readiness startup timed out")
        if codex_result.returncode != 0:
            fail(
                "codex_startup",
                f"canonical Codex readiness startup exited {codex_result.returncode}",
            )
    except CodexRuntimeSyncError as exc:
        primary_error = exc
    finally:
        try:
            _cleanup_runtime_tempdir(temp_workdir)
        except OSError as exc:
            if primary_error is not None:
                primary_error.evidence["cleanup_error"] = str(exc)
            else:
                fail("codex_tempdir_cleanup", f"could not clean Codex runtime temporary cwd: {exc}")
    if primary_error is not None:
        raise primary_error

    try:
        runtime = resolve_codex_runtime(
            home=home,
            cache_path=cache_path,
            competing_package_paths=competitors,
        )
    except CodexRuntimeError as exc:
        fail("final_validation", str(exc))
    evidence["resolver"] = _runtime_payload(runtime)
    if codex_result.stdout.strip() != CODEX_RUNTIME_READY:
        fail("readiness", "canonical Codex did not return the exact readiness token")

    evidence["status"] = "pass"
    evidence["phase"] = "complete"
    evidence["readiness_token"] = CODEX_RUNTIME_READY
    evidence["package_version"]["after"] = runtime.version  # type: ignore[index]
    evidence["cache"]["after"] = _cache_evidence(cache_path)  # type: ignore[index]
    return evidence


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--sync", action="store_true")
    args = parser.parse_args(argv)
    if args.sync:
        try:
            payload = sync_codex_runtime()
        except CodexRuntimeSyncError as exc:
            payload = exc.evidence
            if args.json:
                print(json.dumps(payload, sort_keys=True))
            else:
                print(f"Codex runtime sync FAIL ({payload['phase']}): {exc}")
            return 2
        if args.json:
            print(json.dumps(payload, sort_keys=True))
        else:
            print(f"Codex runtime sync PASS: {payload['resolver']}")
        return 0
    try:
        runtime = resolve_codex_runtime()
    except CodexRuntimeError as exc:
        payload = {"status": "fail", "error": str(exc)}
        if args.json:
            print(json.dumps(payload, sort_keys=True))
        else:
            print(f"Codex runtime FAIL: {exc}")
        return 2
    payload = _runtime_payload(runtime)
    if args.json:
        print(json.dumps(payload, sort_keys=True))
    else:
        print(f"Codex runtime PASS: {runtime.executable} ({runtime.version})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
