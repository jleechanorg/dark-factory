"""Static, fail-closed contract for the one supported Codex CLI runtime."""

from __future__ import annotations

import argparse
import json
import os
from dataclasses import asdict, dataclass
from datetime import datetime
from pathlib import Path
from typing import Iterable


PINNED_CODEX_VERSION = "0.146.0"
PINNED_NODE_VERSION = "v22.22.0"
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


def resolve_codex_executable() -> str:
    """Return the only executable allowed for a Dark Factory Codex process."""
    return str(resolve_codex_runtime().executable)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        runtime = resolve_codex_runtime()
    except CodexRuntimeError as exc:
        payload = {"status": "fail", "error": str(exc)}
        if args.json:
            print(json.dumps(payload, sort_keys=True))
        else:
            print(f"Codex runtime FAIL: {exc}")
        return 2
    payload = {"status": "pass", **{key: str(value) for key, value in asdict(runtime).items()}}
    if args.json:
        print(json.dumps(payload, sort_keys=True))
    else:
        print(f"Codex runtime PASS: {runtime.executable} ({runtime.version})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
