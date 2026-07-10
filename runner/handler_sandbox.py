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
  * `_sealed_benchmark_doc_names` — the operator-only docs inside
    ``benchmarks/<name>/`` whose content the implementing agent must not
    see (sealed design notes, scoring rubrics, scenario catalogs).
  * `_sealed_benchmark_doc_paths` — enumerate the absolute paths of those
    docs under a given implementing-agent workdir.
  * `_sandboxed_args` — prepend ``sandbox-exec -p <profile>`` with holdout
    deny rules to an argv (sealed sibling repo only; legacy / AO backend).
  * `_sandboxed_args_for_workdir` — like ``_sandboxed_args`` but ALSO
    denies the sealed benchmark docs inside the implementing agent's
    workdir. Use this for coder subprocesses (claude, codex, agy) that
    run inside ``ctx.workdir``.

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
from typing import Optional, Union


# Operator-only benchmark docs that the implementing agent must not read.
# These are sealed design notes / scoring rubrics / scenario catalogs.
# ``visible_acceptance.md`` and ``spec.md`` are intentionally absent —
# those are the *visible* contract the agent IS allowed to see.
_SEALED_BENCHMARK_DOC_NAMES = ("README.md", "DESIGN.md", "SCORING.md", "SCENARIOS.md")


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
    default = pathlib.Path.home() / "projects" / "dark-factory-holdouts"
    repo = os.environ.get("DARK_FACTORY_HOLDOUTS", str(default))
    resolved = pathlib.Path(repo).expanduser().resolve()
    if not resolved.is_dir():
        raise RuntimeError(
            f"Sealed holdouts repo not found at {resolved!s}. The implementing-agent "
            "sandbox's deny-list depends on this path existing — silently continuing "
            "would run the agent with an ineffective (empty) deny rule, defeating the "
            "isolation guarantee. Set DARK_FACTORY_HOLDOUTS to the sealed sibling repo's "
            "real location, or clone it to the default path above."
        )
    return resolved


def _holdout_denied_paths() -> list[pathlib.Path]:
    paths = {_holdouts_repo_path()}
    paths.add((pathlib.Path.home() / "projects" / "dark-factory-holdouts").resolve())
    return sorted(paths, key=lambda p: str(p))


def _sealed_benchmark_doc_paths(workdir: "Union[pathlib.Path, str, None]") -> list[pathlib.Path]:
    """Enumerate the operator-only sealed docs under ``<workdir>/benchmarks/*/``.

    These are the design notes / scoring rubrics / scenario catalogs that
    the implementing agent must NOT see. The list is best-effort: a missing
    or malformed workdir returns an empty list, and so does a worktree
    without a ``benchmarks/`` subtree. Used by
    ``_sandboxed_args_for_workdir`` to extend the sandbox-exec deny rules.

    Skips the deny when ``workdir`` is empty, relative, traversing, or does
    not exist — mirrors the ``_capture_diff`` defense-in-depth style.
    """
    if not workdir:
        return []
    try:
        wd_path = pathlib.Path(str(workdir)).resolve()
    except (OSError, RuntimeError):
        return []
    if not wd_path.is_absolute() or ".." in wd_path.parts or not wd_path.is_dir():
        return []
    bench_dir = wd_path / "benchmarks"
    if not bench_dir.is_dir():
        return []
    paths: list[pathlib.Path] = []
    try:
        for child in bench_dir.iterdir():
            if not child.is_dir():
                continue
            for name in _SEALED_BENCHMARK_DOC_NAMES:
                candidate = child / name
                if candidate.is_file():
                    paths.append(candidate.resolve())
    except (OSError, PermissionError):
        return []
    return sorted(paths, key=lambda p: str(p))


def _build_sandbox_profile(extra_denied_paths: list[pathlib.Path]) -> str:
    """Compose a sandbox-exec profile that denies holdouts + extra paths.

    Each extra path gets a file-read* + file-write* deny on its absolute
    path so the deny applies to the operator-only doc file.
    """
    denies: list[str] = []
    for path in _holdout_denied_paths():
        escaped = str(path).replace("\\", "\\\\").replace('"', '\\"')
        denies.append(f'(deny file-read* (subpath "{escaped}"))')
        denies.append(f'(deny file-write* (subpath "{escaped}"))')
    for path in extra_denied_paths:
        escaped = str(path).replace("\\", "\\\\").replace('"', '\\"')
        denies.append(f'(deny file-read* (subpath "{escaped}"))')
        denies.append(f'(deny file-write* (subpath "{escaped}"))')
    deny_rules = "\n".join(denies)
    return f"""
(version 1)
(allow default)
{deny_rules}
"""


def _sandboxed_args(args: list[str]) -> Optional[list[str]]:
    """Prepend ``sandbox-exec`` with the legacy holdout deny rules.

    Legacy / AO backend path: only denies ``$DARK_FACTORY_HOLDOUTS`` (the
    sealed sibling repo). Does NOT deny benchmark docs inside the
    implementing agent's worktree. Use ``_sandboxed_args_for_workdir``
    for coder backends that operate inside ``ctx.workdir``.
    """
    # Skip sandbox if DISABLE_SANDBOX env is set (for testing)
    if os.environ.get("DISABLE_SANDBOX"):
        return args
    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        return None
    profile = _build_sandbox_profile([])
    return [sandbox_exec, "-p", profile] + args


def _sandboxed_args_for_workdir(
    args: list[str], workdir: "Union[pathlib.Path, str, None]"
) -> Optional[list[str]]:
    """Prepend ``sandbox-exec`` with holdout + sealed benchmark doc deny rules.

    Use this for implementing-agent coder backends (claude, codex, agy)
    that run inside ``ctx.workdir``. The deny rules cover:

      * ``$DARK_FACTORY_HOLDOUTS`` (the sealed sibling repo).
      * every ``<workdir>/benchmarks/*/{README,DESIGN,SCORING,SCENARIOS}.md``
        file that exists at call time — these are operator-only sealed
        docs whose contents the agent must not see.

    When ``workdir`` is empty, missing, relative, or traverses, only the
    legacy holdout deny rules apply (same as ``_sandboxed_args``). This
    matches the ``_capture_diff`` defense-in-depth style and prevents a
    forged workdir from leaking deny rules to attacker-controlled paths.
    """
    # Skip sandbox if DISABLE_SANDBOX env is set (for testing)
    if os.environ.get("DISABLE_SANDBOX"):
        return args
    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        return None
    sealed_docs = _sealed_benchmark_doc_paths(workdir)
    profile = _build_sandbox_profile(sealed_docs)
    return [sandbox_exec, "-p", profile] + args
