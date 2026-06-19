"""Holdout-aware sandbox-exec env + argv construction.

Owns:
  * `_sanitized_env` — strip ``DARK_FACTORY_HOLDOUTS`` + any ``*HOLDOUT*`` env
    var so holdout content can never reach a spawned subprocess.
  * `_get_claude_executable` — PATH-first claude binary resolver with a
    nvm-fallback convenience lookup.
  * `_holdouts_repo_path` — resolve ``$DARK_FACTORY_HOLDOUTS`` (defaulting to
    the canonical sibling repo).
  * `_holdout_denied_paths` — list of absolute paths to deny under
    sandbox-exec.
  * `_sandboxed_args` — prepend ``sandbox-exec -p <profile>`` with holdout
    deny rules to an argv.

Note: tests heavily monkeypatch ``runner.handlers._sanitized_env`` and
``runner.handlers._sandboxed_args`` via
``monkeypatch.setattr("runner.handlers._X", ...)``. The ``runner/handlers.py``
shim re-exports these names, and the production callers in this codebase
look them up via ``import runner.handlers as _h`` (late binding) so the
monkeypatches stay in effect.
"""

from __future__ import annotations

import os
import pathlib
import shutil
from typing import Optional


def _sanitized_env() -> dict[str, str]:
    env = {}
    for k, v in os.environ.items():
        if k == "DARK_FACTORY_HOLDOUTS":
            continue
        if "HOLDOUT" in k.upper():
            continue
        env[k] = v
    return env


def _get_claude_executable() -> str:
    # PATH wins so tests can intercept with a fake claude binary on PATH
    # (see tests/test_gates.py::test_gate_nonzero_returncode_cannot_spoof_pass).
    # If nothing on PATH, fall back to the user's nvm-installed binary as a
    # convenience so live runs don't depend on PATH being just-so.
    on_path = shutil.which("claude")
    if on_path:
        return on_path
    nvm_claude = pathlib.Path.home() / ".nvm" / "versions" / "node" / "v22.22.0" / "bin" / "claude"
    if nvm_claude.exists():
        return str(nvm_claude)
    return "claude"



def _holdouts_repo_path() -> pathlib.Path:
    repo = os.environ.get(
        "DARK_FACTORY_HOLDOUTS",
        str(pathlib.Path.home() / "projects" / "dark-factory-holdouts"),
    )
    return pathlib.Path(repo).expanduser().resolve()


def _holdout_denied_paths() -> list[pathlib.Path]:
    paths = {_holdouts_repo_path()}
    paths.add((pathlib.Path.home() / "projects" / "dark-factory-holdouts").resolve())
    return sorted(paths, key=lambda p: str(p))


def _sandboxed_args(args: list[str]) -> Optional[list[str]]:
    # Skip sandbox if DISABLE_SANDBOX env is set (for testing)
    if os.environ.get("DISABLE_SANDBOX"):
        return args
    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        return None
    denies = []
    for path in _holdout_denied_paths():
        holdouts_repo = str(path).replace("\\", "\\\\").replace('"', '\\"')
        denies.append(f'(deny file-read* (subpath "{holdouts_repo}"))')
        denies.append(f'(deny file-write* (subpath "{holdouts_repo}"))')
    deny_rules = "\n".join(denies)
    profile = f"""
(version 1)
(allow default)
{deny_rules}
"""
    return [sandbox_exec, "-p", profile] + args
