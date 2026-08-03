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
  * `_sandboxed_args` — prepend the platform sandbox wrapper (macOS:
    ``sandbox-exec -p <profile>``; Linux: the LD_PRELOAD deny-path shim,
    see below) with holdout deny rules to an argv (sealed sibling repo
    only; legacy / AO backend).
  * `_sandboxed_args_for_workdir` — like ``_sandboxed_args`` but ALSO
    denies the sealed benchmark docs inside the implementing agent's
    workdir. Use this for coder subprocesses (claude, codex, agy) that
    run inside ``ctx.workdir``.

Linux isolation backend (jleechan-haux)
----------------------------------------

macOS's ``sandbox-exec`` has no portable Linux equivalent that is usable
without elevated privilege. The obvious candidates were tried and rejected
empirically (not theoretically) against a locked-down Linux host
(``kernel.apparmor_restrict_unprivileged_userns=1``, the Ubuntu 24.04
default):

  * **bubblewrap (bwrap)** requires either a setuid-root binary or a
    working unprivileged user namespace to create ANY namespace
    (including the mount namespace used for path-masking). On a host
    where unprivileged user namespaces are blocked, every ``bwrap``
    invocation — even ones that don't touch networking — fails with
    ``setting up uid map: Permission denied`` before doing anything.
  * **``systemd-run --user --scope -p InaccessiblePaths=...``** looks
    like it should work (exits 0, no error text) but the property is
    silently dropped for unprivileged ``--user`` transient units on the
    same locked-down host: ``systemctl --user show <unit> -p
    InaccessiblePaths`` comes back **empty** after the run, and the
    "denied" path was fully readable inside the unit. Trusting the exit
    code here would repeat the "gate self-certification" mistake (a
    check whose expected value comes from its own unverified assumption
    can't fail) — so this backend is **not** used.

Instead, the Linux backend is a small **LD_PRELOAD deny-path shim**
(``scripts/agent-isolation/deny_paths_preload.c``, compiled on demand and
cached under ``~/.cache/dark-factory/agent-isolation/``). It intercepts
``open``/``open64``/``openat``/``openat64``/``fopen``/``fopen64`` and
returns ``ENOENT`` for any resolved path under a colon-separated
``DENY_PATHS`` list. This requires no kernel privilege, no namespaces, and
no setuid — just a working C compiler and a dynamically-linked target
process (see the module docstring in the ``.c`` file for the scope
limitations: statically-linked or setuid binaries bypass it).

Every use of this backend is verified **behaviorally** at runtime
(``_verify_linux_preload_denies``) — an actual denied read is attempted
and must fail — rather than trusted from the compiler/loader's exit code
alone. If verification fails (or the compiler/library are unavailable),
``_sandboxed_args``/``_sandboxed_args_for_workdir`` return ``None`` and
callers fail the node closed (see ``runner/handler_codergen.py``), exactly
as they already did for a missing ``sandbox-exec`` on macOS.

Note: tests heavily monkeypatch ``runner.handlers._sanitized_env`` and
``runner.handlers._sandboxed_args`` via
``monkeypatch.setattr("runner.handlers._X", ...)``. The ``runner/handlers.py``
shim re-exports these names, and the production callers in this codebase
look them up via ``import runner.handlers as _h`` (late binding) so the
monkeypatches stay in effect.
"""

from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
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


def _build_sandbox_profile(
    extra_denied_paths: list[pathlib.Path],
    extra_write_denied_paths: list[pathlib.Path] | None = None,
) -> str:
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
    for path in extra_write_denied_paths or []:
        escaped = str(path).replace("\\", "\\\\").replace('"', '\\"')
        denies.append(f'(deny file-write* (subpath "{escaped}"))')
    deny_rules = "\n".join(denies)
    return f"""
(version 1)
(allow default)
{deny_rules}
"""


# ---------------------------------------------------------------------------
# Linux backend — LD_PRELOAD deny-path shim (jleechan-haux).
#
# See the module docstring for why bwrap / systemd-run were tried and
# rejected on a real locked-down host. This backend needs a C compiler and
# a dynamically-linked target; verification is behavioral, not exit-code.
# ---------------------------------------------------------------------------

_LINUX_PRELOAD_SOURCE = (
    pathlib.Path(__file__).resolve().parent.parent
    / "scripts"
    / "agent-isolation"
    / "deny_paths_preload.c"
)

# Process-lifetime cache: None = not yet checked, True/False = last result.
# Reset via `_reset_linux_preload_verification_cache_for_tests()` in tests.
#
# NOT keyed by lib_path (deliberate, but worth understanding the tradeoff,
# flagged by independent review of PR #233): within one runner process
# there is only ever one content-hashed .so on disk at a time (the source
# file doesn't change mid-run), so this is benign in production. The
# failure mode to know about: if the FIRST canary check hits a transient
# error (e.g. /tmp momentarily full, or the 10s subprocess timeout under
# heavy host load), the cache latches to False for the rest of the
# process's life, and every codergen node after that fails closed with
# "isolation unavailable" until the runner restarts. That's the safe
# direction to fail in (no node ever proceeds unsandboxed), but it is an
# availability cliff, not just a security one — if Linux codergen nodes
# start failing closed in a burst, check for a transient canary flake
# before assuming a real regression.
_linux_preload_verified: "Optional[bool]" = None


def _linux_preload_cache_dir() -> pathlib.Path:
    return pathlib.Path.home() / ".cache" / "dark-factory" / "agent-isolation"


def _linux_preload_lib_path() -> "Optional[pathlib.Path]":
    """Build (once, content-hash cached) the LD_PRELOAD deny-path shim.

    Returns ``None`` — never a partially-built or stale artifact — when the
    source is missing, no C compiler is available, or compilation fails.
    Callers MUST treat ``None`` as "no Linux isolation backend available"
    and fail the node closed; never fall back to running the subprocess
    unsandboxed.
    """
    if not _LINUX_PRELOAD_SOURCE.is_file():
        return None
    compiler = shutil.which("cc") or shutil.which("gcc")
    if compiler is None:
        return None
    try:
        digest = hashlib.sha256(_LINUX_PRELOAD_SOURCE.read_bytes()).hexdigest()[:16]
    except OSError:
        return None
    cache_dir = _linux_preload_cache_dir()
    target = cache_dir / f"deny_paths_preload-{digest}.so"
    if target.is_file():
        return target
    try:
        cache_dir.mkdir(parents=True, exist_ok=True)
        proc = subprocess.run(
            [
                compiler,
                "-shared",
                "-fPIC",
                "-O2",
                "-Wno-format-truncation",
                "-o",
                str(target),
                str(_LINUX_PRELOAD_SOURCE),
                "-ldl",
            ],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0 or not target.is_file():
        return None
    return target


def _verify_linux_preload_denies(lib_path: "pathlib.Path") -> bool:
    """Behaviorally verify the shim actually denies a marked-off path.

    Do NOT trust "the helper process exited 0" as proof of containment —
    ``systemd-run --user --scope -p InaccessiblePaths=...`` does exactly
    that (exits 0, applies nothing) on a host without unprivileged user
    namespaces. Instead, create a real canary file under a fresh temp dir,
    put that dir in DENY_PATHS, and confirm a real subprocess actually
    fails to read it with the shim loaded.
    """
    global _linux_preload_verified
    if _linux_preload_verified is not None:
        return _linux_preload_verified
    try:
        with tempfile.TemporaryDirectory(prefix="df-iso-canary-") as tmp:
            marker = pathlib.Path(tmp) / "canary.txt"
            marker.write_text("canary-secret", encoding="utf-8")
            env = dict(os.environ)
            env["LD_PRELOAD"] = str(lib_path)
            env["DENY_PATHS"] = tmp
            cat = shutil.which("cat") or "/bin/cat"
            proc = subprocess.run(
                [cat, str(marker)],
                capture_output=True,
                text=True,
                timeout=10,
                check=False,
                env=env,
            )
            # Denial must (a) fail the read and (b) never leak the content.
            _linux_preload_verified = proc.returncode != 0 and "canary-secret" not in proc.stdout
    except (OSError, subprocess.SubprocessError):
        _linux_preload_verified = False
    return bool(_linux_preload_verified)


def _reset_linux_preload_verification_cache_for_tests() -> None:
    """Test-only: clear the process-lifetime verification cache."""
    global _linux_preload_verified
    _linux_preload_verified = None


def _linux_sandbox_prefix(denied_paths: "list[pathlib.Path]") -> "Optional[list[str]]":
    """Build the ``env LD_PRELOAD=... DENY_PATHS=...`` argv prefix.

    Returns ``None`` (fail closed) when the shim can't be built or its
    deny behavior can't be verified on this host.
    """
    lib = _linux_preload_lib_path()
    if lib is None:
        return None
    if not _verify_linux_preload_denies(lib):
        return None
    joined = ":".join(str(p) for p in denied_paths)
    env_bin = shutil.which("env") or "/usr/bin/env"
    return [env_bin, f"LD_PRELOAD={lib}", f"DENY_PATHS={joined}"]


_darwin_sandbox_exec_verified: "Optional[bool]" = None


def _verify_darwin_sandbox_exec() -> bool:
    """Test whether sandbox-exec can apply profiles on this macOS host.

    When executing inside an existing sandbox (e.g., Antigravity agent CLI),
    sandbox-exec fails with code 71 (`sandbox-exec: sandbox_apply: Operation not permitted`).
    This canary check runs a minimal sandbox profile and caches the result for the
    process lifetime.
    """
    global _darwin_sandbox_exec_verified
    if os.environ.get("DARK_FACTORY_OUTER_SANDBOX") == "1":
        return True
    if _darwin_sandbox_exec_verified is not None:
        return _darwin_sandbox_exec_verified
    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        _darwin_sandbox_exec_verified = False
        return False
    true_bin = shutil.which("true") or "/usr/bin/true"
    try:
        proc = subprocess.run(
            [sandbox_exec, "-p", "(version 1)\n(allow default)", true_bin],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        _darwin_sandbox_exec_verified = proc.returncode == 0
    except (OSError, subprocess.SubprocessError):
        _darwin_sandbox_exec_verified = False
    return bool(_darwin_sandbox_exec_verified)


def _reset_darwin_sandbox_verification_cache_for_tests() -> None:
    """Test-only: clear the process-lifetime macOS sandbox verification cache."""
    global _darwin_sandbox_exec_verified
    _darwin_sandbox_exec_verified = None


def _sandboxed_args(args: list[str]) -> Optional[list[str]]:
    """Prepend the platform sandbox wrapper with the legacy holdout deny rules.

    Legacy / AO backend path: only denies ``$DARK_FACTORY_HOLDOUTS`` (the
    sealed sibling repo). Does NOT deny benchmark docs inside the
    implementing agent's worktree. Use ``_sandboxed_args_for_workdir``
    for coder backends that operate inside ``ctx.workdir``.

    macOS uses ``sandbox-exec``; Linux uses the LD_PRELOAD deny-path shim
    (see module docstring). Any other platform, or a platform backend that
    can't be built/verified, returns ``None`` — fail closed.
    """
    # Skip sandbox if DISABLE_SANDBOX env is set (for testing)
    if os.environ.get("DISABLE_SANDBOX"):
        return args
    if sys.platform == "darwin":
        if not _verify_darwin_sandbox_exec():
            return None
        sandbox_exec = shutil.which("sandbox-exec")
        if sandbox_exec is None:
            return None
        profile = _build_sandbox_profile([])
        return [sandbox_exec, "-p", profile] + args
    if sys.platform.startswith("linux"):
        prefix = _linux_sandbox_prefix(_holdout_denied_paths())
        if prefix is None:
            return None
        return prefix + args
    return None

def _sandboxed_args_for_workdir(
    args: list[str], workdir: "Union[pathlib.Path, str, None]"
) -> Optional[list[str]]:
    """Prepend the platform sandbox wrapper with holdout + sealed-doc deny rules.

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

    macOS uses ``sandbox-exec``; Linux uses the LD_PRELOAD deny-path shim
    (see module docstring). Any other platform, or a platform backend that
    can't be built/verified, returns ``None`` — fail closed.
    """
    # Skip sandbox if DISABLE_SANDBOX env is set (for testing)
    if os.environ.get("DISABLE_SANDBOX"):
        return args
    if sys.platform == "darwin":
        if not _verify_darwin_sandbox_exec():
            return None
        sandbox_exec = shutil.which("sandbox-exec")
        if sandbox_exec is None:
            return None
        sealed_docs = _sealed_benchmark_doc_paths(workdir)
        write_denied: list[pathlib.Path] = []
        if workdir:
            candidate = pathlib.Path(workdir)
            if candidate.is_absolute() and ".." not in candidate.parts:
                venv = candidate / ".venv"
                if venv.is_dir() and not venv.is_symlink():
                    write_denied.append(venv.resolve())
        profile = _build_sandbox_profile(sealed_docs, write_denied)
        return [sandbox_exec, "-p", profile] + args
    if sys.platform.startswith("linux"):
        sealed_docs = _sealed_benchmark_doc_paths(workdir)
        prefix = _linux_sandbox_prefix(_holdout_denied_paths() + sealed_docs)
        if prefix is None:
            return None
        return prefix + args
    return None
