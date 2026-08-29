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

The Linux **controller reviewer** uses ``landlock_launcher.c`` in addition to
this shim. It installs a kernel-enforced read/write allow-list before native
Codex starts, so static binaries and direct syscalls cannot bypass the sealed
holdout boundary. The preload shim is retained only as defense-in-depth; if
the launcher cannot be built or Landlock is unavailable, controller launch
fails closed.

Note: tests heavily monkeypatch ``runner.handlers._sanitized_env`` and
``runner.handlers._sandboxed_args`` via
``monkeypatch.setattr("runner.handlers._X", ...)``. The ``runner/handlers.py``
shim re-exports these names, and the production callers in this codebase
look them up via ``import runner.handlers as _h`` (late binding) so the
monkeypatches stay in effect.
"""

from __future__ import annotations

import hashlib
import ctypes
import grp
import os
import pathlib
import pwd
import shutil
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from typing import Optional, Union


# Operator-only benchmark docs that the implementing agent must not read.
# These are sealed design notes / scoring rubrics / scenario catalogs.
# ``visible_acceptance.md`` and ``spec.md`` are intentionally absent —
# those are the *visible* contract the agent IS allowed to see.
_SEALED_BENCHMARK_DOC_NAMES = ("README.md", "DESIGN.md", "SCORING.md", "SCENARIOS.md")


@dataclass(frozen=True)
class _ControllerRuntime:
    """Private per-review Codex home and its exact cleanup root."""

    run_dir: pathlib.Path
    codex_home: pathlib.Path
    env: dict[str, str]


_CONTROLLER_OUTPUT_SCHEMA = (
    '{"type":"object","additionalProperties":false,"required":'
    '["verdict","findings","evidence_checked","commands_executed","caveats"],'
    '"properties":{"verdict":{"enum":["pass","fail"]},'
    '"findings":{"type":"array","items":{"type":"string"}},'
    '"evidence_checked":{"type":"array","items":{"type":"string"}},'
    '"commands_executed":{"type":"array","items":{"type":"string"}},'
    '"caveats":{"type":"array","items":{"type":"string"}}}}\n'
)


def _controller_output_schema(run_dir: pathlib.Path) -> pathlib.Path:
    """Create the immutable, controller-owned response schema for one run."""
    run_dir = _validate_private_dir(pathlib.Path(run_dir))
    path = run_dir / "output-schema.json"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, 0o444)
    try:
        os.write(fd, _CONTROLLER_OUTPUT_SCHEMA.encode("utf-8"))
        os.fsync(fd)
        os.fchmod(fd, 0o444)
    finally:
        os.close(fd)
    info = path.lstat()
    if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o444:
        raise ValueError("controller output schema is not immutable")
    return path


def _validate_private_dir(path: pathlib.Path) -> pathlib.Path:
    """Validate an existing absolute directory without following symlinks."""
    path = pathlib.Path(path)
    if not path.is_absolute():
        raise ValueError("controller runtime path must be absolute")
    current = pathlib.Path(path.anchor)
    for component in path.parts[1:]:
        current /= component
        info = current.lstat()
        if (
            stat.S_ISLNK(info.st_mode)
            or not stat.S_ISDIR(info.st_mode)
            or info.st_uid not in {0, os.getuid()}
            or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
        ):
            raise ValueError(f"controller runtime path is not private: {current}")
    return path


def _ensure_private_dir(path: pathlib.Path) -> pathlib.Path:
    """Create each missing component privately, validating every result."""
    path = pathlib.Path(path)
    if not path.is_absolute():
        raise ValueError("controller runtime path must be absolute")
    current = pathlib.Path(path.anchor)
    repair_allowed = False
    for component in path.parts[1:]:
        current /= component
        repair_allowed = repair_allowed or current.name == ".dark-factory"
        try:
            info = current.lstat()
        except FileNotFoundError:
            current.mkdir(mode=0o700)
        else:
            # Existing user-owned runtime roots may predate this contract and
            # be group/other-writable. Tighten only the .dark-factory subtree
            # before validation; HOME and its ancestors must already be
            # private. Symlinks, files, and foreign-owned paths remain
            # fail-closed through _validate_private_dir.
            if (
                repair_allowed
                and stat.S_ISDIR(info.st_mode)
                and info.st_uid == os.getuid()
                and info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
            ):
                os.chmod(current, 0o700, follow_symlinks=False)
        _validate_private_dir(current)
    return path


def _copy_controller_auth(source: pathlib.Path, destination: pathlib.Path) -> None:
    """Copy one validated auth file into a fresh private Codex home."""
    _validate_private_dir(source.parent)
    source_info = source.lstat()
    if (
        not stat.S_ISREG(source_info.st_mode)
        or source_info.st_nlink != 1
        or source_info.st_uid != os.getuid()
        or stat.S_IMODE(source_info.st_mode) != 0o600
    ):
        raise ValueError("controller Codex auth source is not a private regular file")
    if destination.exists() or destination.is_symlink():
        raise ValueError("controller Codex auth destination already exists")
    source_flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        source_flags |= os.O_NOFOLLOW
    source_fd = os.open(source, source_flags)
    try:
        source_info = os.fstat(source_fd)
        if (
            not stat.S_ISREG(source_info.st_mode)
            or source_info.st_uid != os.getuid()
            or source_info.st_nlink != 1
            or stat.S_IMODE(source_info.st_mode) != 0o600
        ):
            raise ValueError("controller Codex auth source is not a private regular file")
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        fd = os.open(destination, flags, 0o600)
        try:
            while True:
                chunk = os.read(source_fd, 1024 * 1024)
                if not chunk:
                    break
                view = memoryview(chunk)
                while view:
                    written = os.write(fd, view)
                    view = view[written:]
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        os.close(source_fd)
    destination_info = destination.lstat()
    if (
        stat.S_ISLNK(destination_info.st_mode)
        or not stat.S_ISREG(destination_info.st_mode)
        or destination_info.st_uid != os.getuid()
        or destination_info.st_nlink != 1
        or stat.S_IMODE(destination_info.st_mode) != 0o600
    ):
        raise ValueError("controller Codex auth destination is not private")


def _create_controller_runtime() -> _ControllerRuntime:
    """Create an isolated, writable-only Codex runtime for one review."""
    home = pathlib.Path.home()
    parent = _ensure_private_dir(home / ".dark-factory" / "controller-runtimes")
    run_dir = pathlib.Path(tempfile.mkdtemp(prefix="review-", dir=str(parent)))
    try:
        _validate_private_dir(run_dir)
        codex_home = _ensure_private_dir(run_dir / "codex-home")
        _ensure_private_dir(codex_home / "tmp")
        configured_codex_home = os.environ.get("CODEX_HOME")
        if configured_codex_home is None:
            auth_source_root = home / ".codex"
        else:
            if not configured_codex_home:
                raise ValueError("configured CODEX_HOME must be an absolute directory")
            auth_source_root = pathlib.Path(configured_codex_home)
            if not auth_source_root.is_absolute():
                raise ValueError("configured CODEX_HOME must be an absolute directory")
        _validate_private_dir(auth_source_root)
        _copy_controller_auth(
            auth_source_root / "auth.json", codex_home / "auth.json"
        )
    except Exception:
        try:
            _validate_private_dir(run_dir)
            shutil.rmtree(run_dir)
        except Exception:  # noqa: BLE001, S110 - runtime cleanup is best-effort
            pass
        raise
    env = _sanitized_env()
    env.update(
        {
            "CODEX_HOME": str(codex_home),
            "HOME": str(codex_home),
            "TMPDIR": str(codex_home / "tmp"),
        }
    )
    return _ControllerRuntime(run_dir=run_dir, codex_home=codex_home, env=env)


def _cleanup_controller_runtime(run_dir: pathlib.Path) -> None:
    """Remove only an owned, validated per-review runtime directory."""
    run_dir = _validate_private_dir(pathlib.Path(run_dir))
    if not run_dir.name.startswith("review-"):
        raise ValueError("controller runtime cleanup target is not a review run")
    parent = _validate_private_dir(run_dir.parent)
    if parent.name != "controller-runtimes":
        raise ValueError("controller runtime cleanup parent is invalid")
    shutil.rmtree(run_dir)
    if run_dir.exists() or run_dir.is_symlink():
        raise OSError("controller runtime cleanup did not remove the run directory")


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


def _macos_read_only_profile(
    profile: str,
    read_only_path: pathlib.Path | str | None = None,
    writable_path: pathlib.Path | str | None = None,
) -> str:
    """Add the controller's read-only write boundary to an existing profile.

    Controller Codex runs under this outer Seatbelt profile on macOS. Codex's
    own sandbox must therefore be bypassed (the outer profile remains the
    security boundary), while the reviewed workspace is denied writes. The
    profile must already contain a path-specific holdout read denial; an
    incomplete profile is rejected rather than upgraded into a weaker one.
    """
    if "(deny file-read* (subpath \"" not in profile:
        raise ValueError("controller sandbox profile lacks holdout read denial")
    if read_only_path is None:
        raise ValueError("controller sandbox profile lacks target read denial")
    try:
        target = pathlib.Path(read_only_path).resolve(strict=True)
    except (OSError, RuntimeError) as exc:
        raise ValueError("controller sandbox target is unavailable") from exc
    escaped_target = str(target).replace("\\", "\\\\").replace('"', '\\"')
    profile = profile.rstrip() + f'\n(deny file-read* (subpath "{escaped_target}"))\n'
    write_rule = "(deny file-write*)"
    profile = profile.rstrip() + "\n" + write_rule + "\n"
    # Shells and Git use /dev/null for ordinary command plumbing. It is a
    # device sink, not a writable filesystem location; allowing it keeps the
    # reviewer transport functional without opening any user-controlled path.
    profile += '(allow file-write* (literal "/dev/null"))\n'
    if writable_path is not None:
        writable = _validate_private_dir(pathlib.Path(writable_path))
        escaped = str(writable).replace("\\", "\\\\").replace('"', '\\"')
        profile += f'(allow file-write* (subpath "{escaped}"))\n'
    return profile


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


# Linux controller reviews need kernel enforcement because a reviewer may use
# a static executable or issue raw openat(2) syscalls.  The preload shim above
# remains in the prefix as defense-in-depth, but it is not the boundary.
_LINUX_LANDLOCK_SOURCE = (
    pathlib.Path(__file__).resolve().parent.parent
    / "scripts"
    / "agent-isolation"
    / "landlock_launcher.c"
)
_linux_landlock_launcher: pathlib.Path | None = None
_linux_landlock_launcher_checked = False


class _PinnedLauncherCommand(list[str]):
    """Command argv carrying the already-verified launcher descriptor."""

    def __init__(self, args: list[str], launcher_fd: int):
        super().__init__(args)
        self.launcher_fd = launcher_fd
        self.pass_fds = (launcher_fd,)

    def close_launcher(self) -> None:
        if self.launcher_fd >= 0:
            os.close(self.launcher_fd)
            self.launcher_fd = -1
            self.pass_fds = ()

    def __add__(self, other: list[str]) -> _PinnedLauncherCommand:
        return _PinnedLauncherCommand(list(self) + list(other), self.launcher_fd)

    def __radd__(self, other: list[str]) -> _PinnedLauncherCommand:
        return _PinnedLauncherCommand(list(other) + list(self), self.launcher_fd)


def _linux_landlock_abi() -> int | None:
    """Return the host Landlock ABI, or None when the syscall is unavailable."""
    if not sys.platform.startswith("linux"):
        return None
    syscall_nr = {"x86_64": 444, "aarch64": 444, "riscv64": 444}.get(
        os.uname().machine
    )
    if syscall_nr is None:
        return None
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        libc.syscall.restype = ctypes.c_long
        abi = libc.syscall(
            ctypes.c_long(syscall_nr),
            ctypes.c_void_p(),
            ctypes.c_size_t(0),
            ctypes.c_uint(1),
        )
    except (AttributeError, OSError):
        return None
    return int(abi) if abi > 0 else None


def _linux_landlock_cache_dir() -> pathlib.Path:
    return pathlib.Path.home() / ".cache" / "dark-factory" / "agent-isolation"


def _prepare_private_cache_dir(path: pathlib.Path) -> pathlib.Path:
    """Create the launcher cache and tighten user-owned parents before use."""
    path = pathlib.Path(path)
    if not path.is_absolute():
        raise ValueError("launcher cache path must be absolute")
    current = pathlib.Path(path.anchor)
    for component in path.parts[1:]:
        current /= component
        try:
            info = current.lstat()
        except FileNotFoundError:
            current.mkdir(mode=0o700)
            info = current.lstat()
        if (
            stat.S_ISLNK(info.st_mode)
            or not stat.S_ISDIR(info.st_mode)
            or info.st_uid not in {0, os.getuid()}
        ):
            raise ValueError(f"launcher cache parent is not trusted: {current}")
        if info.st_uid == os.getuid() and info.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
            current.chmod(0o700)
    return _validate_private_dir(path)


def _private_regular_executable(path: pathlib.Path) -> bool:
    try:
        info = path.lstat()
    except OSError:
        return False
    return (
        stat.S_ISREG(info.st_mode)
        and info.st_nlink == 1
        and info.st_uid == os.getuid()
        and not info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
        and stat.S_IMODE(info.st_mode) & stat.S_IXUSR
    )


def _open_verified_launcher(path: pathlib.Path) -> int | None:
    """Open the validated launcher without following a replacement symlink."""
    flags = getattr(os, "O_PATH", os.O_RDONLY)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(path, flags)
        info = os.fstat(fd)
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_nlink != 1
            or info.st_uid != os.getuid()
            or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
            or not stat.S_IMODE(info.st_mode) & stat.S_IXUSR
        ):
            os.close(fd)
            return None
        content_fd = os.open(f"/proc/self/fd/{fd}", os.O_RDONLY)
        actual_digest = hashlib.sha256()
        offset = 0
        try:
            while True:
                chunk = os.pread(content_fd, 1024 * 1024, offset)
                if not chunk:
                    break
                actual_digest.update(chunk)
                offset += len(chunk)
        finally:
            os.close(content_fd)
        manifest = path.with_name(path.name + ".manifest")
        manifest_flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            manifest_flags |= os.O_NOFOLLOW
        manifest_fd = os.open(manifest, manifest_flags)
        try:
            manifest_info = os.fstat(manifest_fd)
            if (
                not stat.S_ISREG(manifest_info.st_mode)
                or manifest_info.st_nlink != 1
                or manifest_info.st_uid != os.getuid()
                or manifest_info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
            ):
                os.close(fd)
                return None
            manifest_bytes = os.read(manifest_fd, 4096)
        finally:
            os.close(manifest_fd)
        source_digest = hashlib.sha256(_LINUX_LANDLOCK_SOURCE.read_bytes()).hexdigest()
        expected = (
            f"source_sha256={source_digest}\n"
            f"binary_sha256={actual_digest.hexdigest()}\n"
        ).encode("ascii")
        if manifest_bytes != expected:
            os.close(fd)
            return None
        return fd
    except (OSError, UnicodeError):
        try:
            os.close(fd)
        except (UnboundLocalError, OSError):
            pass
        return None


def _extend_pinned_launcher_command(
    prefix: list[str], suffix: list[str]
) -> list[str]:
    """Append argv while retaining the launcher's inherited descriptor."""
    launcher_fd = getattr(prefix, "launcher_fd", None)
    if launcher_fd is None:
        return prefix + suffix
    return _PinnedLauncherCommand(list(prefix) + suffix, launcher_fd)


def _close_pinned_launcher_command(command: object) -> None:
    close = getattr(command, "close_launcher", None)
    if close is not None:
        close()


def _linux_landlock_launcher_path() -> pathlib.Path | None:
    """Build and content-hash cache the kernel-enforced launcher."""
    global _linux_landlock_launcher, _linux_landlock_launcher_checked
    if _linux_landlock_launcher_checked:
        return _linux_landlock_launcher
    _linux_landlock_launcher_checked = True
    if (_linux_landlock_abi() or 0) < 3:
        return None
    compiler = shutil.which("cc") or shutil.which("gcc")
    if compiler is None or not _LINUX_LANDLOCK_SOURCE.is_file():
        return None
    try:
        source_digest = hashlib.sha256(_LINUX_LANDLOCK_SOURCE.read_bytes()).hexdigest()
        digest = source_digest[:16]
        cache_dir = _linux_landlock_cache_dir()
        _prepare_private_cache_dir(cache_dir)
        target = cache_dir / f"landlock-launcher-{digest}"
        manifest = target.with_name(target.name + ".manifest")
        reusable = False
        if _private_regular_executable(target):
            try:
                manifest_info = manifest.lstat()
                manifest_text = manifest.read_text(encoding="ascii").strip().splitlines()
                binary_digest = hashlib.sha256(target.read_bytes()).hexdigest()
                reusable = (
                    stat.S_ISREG(manifest_info.st_mode)
                    and manifest_info.st_nlink == 1
                    and manifest_info.st_uid == os.getuid()
                    and not manifest_info.st_mode & (stat.S_IWGRP | stat.S_IWOTH)
                    and manifest_text == [
                        f"source_sha256={source_digest}",
                        f"binary_sha256={binary_digest}",
                    ]
                )
            except (OSError, UnicodeError):
                reusable = False
        if reusable:
            _linux_landlock_launcher = target
            return target

        temp_fd, temp_name = tempfile.mkstemp(prefix=f".{target.name}.", dir=str(cache_dir))
        temp_path = pathlib.Path(temp_name)
        manifest_temp = cache_dir / f".{manifest.name}.{os.getpid()}"
        try:
            os.close(temp_fd)
            temp_path.chmod(0o700)
            proc = subprocess.run(
                [compiler, "-O2", "-Wall", "-Wextra", "-o", str(temp_path), str(_LINUX_LANDLOCK_SOURCE)],
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            if proc.returncode != 0 or not _private_regular_executable(temp_path):
                return None
            binary_digest = hashlib.sha256(temp_path.read_bytes()).hexdigest()
            manifest_temp_fd = os.open(
                manifest_temp,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL,
                0o600,
            )
            try:
                os.write(
                    manifest_temp_fd,
                    f"source_sha256={source_digest}\nbinary_sha256={binary_digest}\n".encode("ascii"),
                )
                os.fsync(manifest_temp_fd)
            finally:
                os.close(manifest_temp_fd)
            os.replace(temp_path, target)
            os.replace(manifest_temp, manifest)
            if not _private_regular_executable(target):
                return None
            _linux_landlock_launcher = target
            return target
        finally:
            for stale in (temp_path, manifest_temp):
                try:
                    stale.unlink()
                except FileNotFoundError:
                    pass
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def _reset_linux_landlock_launcher_cache_for_tests() -> None:
    global _linux_landlock_launcher, _linux_landlock_launcher_checked
    _linux_landlock_launcher = None
    _linux_landlock_launcher_checked = False


def _linux_codex_runtime_paths(executable: pathlib.Path) -> list[pathlib.Path] | None:
    """Return exact installed Codex roots needed by a JS/native launcher."""
    try:
        resolved = executable.resolve(strict=True)
    except (OSError, RuntimeError):
        return None
    # Refuse executable trees whose package directories are mutable by another
    # user.  A public sticky parent such as /tmp is fine.  Group-writable
    # user-owned npm prefixes are common (for example, nvm installs); those
    # remain bound to the current owner, while world-writable package roots do
    # not enter the allow-list.
    def private_group(info: os.stat_result) -> bool:
        if not info.st_mode & stat.S_IWGRP:
            return True
        try:
            uid = os.getuid()
            gid = os.getgid()
            username = pwd.getpwuid(uid).pw_name
            if info.st_gid != gid:
                return False
            for entry in pwd.getpwall():
                if entry.pw_gid == gid and (entry.pw_uid != uid or entry.pw_name != username):
                    return False
            for entry in grp.getgrall():
                if entry.gr_gid == gid and any(member != username for member in entry.gr_mem):
                    return False
        except (KeyError, OSError, RuntimeError):
            return False
        return True

    def trusted_directory(path: pathlib.Path) -> pathlib.Path | None:
        try:
            current = path.resolve(strict=True)
            if not current.is_dir():
                return None
            while True:
                info = current.lstat()
                if info.st_uid not in {0, os.getuid()}:
                    return None
                if info.st_mode & stat.S_IWOTH:
                    if info.st_uid == 0 and info.st_mode & stat.S_ISVTX:
                        break
                    return None
                if not private_group(info):
                    return None
                if current == pathlib.Path(current.anchor):
                    break
                current = current.parent
            return path.resolve(strict=True)
        except (OSError, RuntimeError):
            return None

    executable_parent = trusted_directory(resolved.parent)
    if executable_parent is None:
        return None
    paths = [executable_parent]

    def add_package_roots(path: pathlib.Path) -> None:
        parts = path.parts
        for index in range(len(parts) - 1):
            if parts[index:index + 2] in (("@openai", "codex"), ("@openai", "codex-linux-x64")):
                package_root = pathlib.Path(*parts[: index + 2])
                package_root = trusted_directory(package_root)
                if package_root is not None:
                    paths.append(package_root)

    add_package_roots(resolved)
    # JS and shell launchers may point at a bundled native executable or a
    # non-system interpreter. Follow only explicit launcher references; do
    # not allow every PATH directory, which could contain a sealed child.
    try:
        first_line = resolved.read_text(encoding="utf-8").splitlines()[0]
    except (OSError, UnicodeDecodeError, IndexError):
        first_line = ""
    if first_line.startswith("#!"):
        interpreter = first_line[2:].strip().split()
        if interpreter and pathlib.Path(interpreter[0]).name == "env" and len(interpreter) > 1:
            interpreter_path = shutil.which(interpreter[1])
        else:
            interpreter_path = interpreter[0] if interpreter else None
        if interpreter_path:
            try:
                interpreter_root = trusted_directory(
                    pathlib.Path(interpreter_path).resolve(strict=True).parent
                )
                if interpreter_root is None:
                    return None
                paths.append(interpreter_root)
            except (OSError, RuntimeError):
                return None
    try:
        for line in resolved.read_text(encoding="utf-8").splitlines():
            if "real-bin:" not in line and not line.startswith("REAL_BIN="):
                continue
            candidate = line.split(":", 1)[1].strip() if "real-bin:" in line else line.split("=", 1)[1].strip()
            candidate_path = pathlib.Path(candidate)
            if candidate_path.is_file():
                candidate_path = candidate_path.resolve(strict=True)
                candidate_root = trusted_directory(candidate_path.parent)
                if candidate_root is None:
                    return None
                paths.append(candidate_root)
                add_package_roots(candidate_path)
    except (OSError, UnicodeDecodeError, RuntimeError):
        return None
    # Codex's JS launcher resolves the platform package with Node's normal
    # module lookup.  npm may hoist that optional package beside the primary
    # package, while pnpm may keep it nested under the primary package.  Add
    # only those exact package locations and retain their mode/owner checks.
    package_roots = [path for path in paths if path.name == "codex"]
    machine = os.uname().machine if hasattr(os, "uname") else ""
    native_name = {
        "x86_64": "codex-linux-x64",
        "amd64": "codex-linux-x64",
        "aarch64": "codex-linux-arm64",
        "arm64": "codex-linux-arm64",
    }.get(machine)
    if native_name:
        for package_root in package_roots:
            candidates = (
                package_root.parent / native_name,
                package_root / "node_modules" / "@openai" / native_name,
            )
            for candidate in candidates:
                try:
                    candidate.lstat()
                except FileNotFoundError:
                    continue
                except OSError:
                    return None
                native_root = trusted_directory(candidate)
                if native_root is None:
                    # An installed native package that cannot be trusted must
                    # fail closed; silently omitting it would let the caller
                    # run with a different, unvalidated runtime.
                    return None
                paths.append(native_root)
    return sorted(set(paths), key=str)


def _linux_controller_sandbox_prefix(
    *,
    denied_paths: list[pathlib.Path],
    read_paths: list[pathlib.Path],
    writable_paths: list[pathlib.Path],
    executable_paths: list[pathlib.Path] | None = None,
) -> list[str] | None:
    """Return a Landlock allow-list prefix for a controller Codex process.

    Landlock is an allow-list API: an omitted path is denied.  Keep the list
    deliberately small while allowing ordinary command execution, the target
    checkout, and the private Codex runtime.  Any path that overlaps a sealed
    path causes a closed failure rather than weakening the rule.
    """
    launcher = _linux_landlock_launcher_path()
    if launcher is None:
        return None

    def normalized(
        paths: list[pathlib.Path], *, strict: bool = True
    ) -> list[pathlib.Path] | None:
        result: list[pathlib.Path] = []
        for raw in paths:
            try:
                path = pathlib.Path(raw).resolve(strict=strict)
            except (OSError, RuntimeError):
                return None
            if not path.is_absolute():
                return None
            result.append(path)
        return result

    denied = normalized(denied_paths, strict=False)
    reads = normalized(read_paths)
    writes = normalized(writable_paths)
    executables = normalized(executable_paths or [])
    if denied is None or reads is None or writes is None or executables is None:
        return None

    def contains(parent: pathlib.Path, child: pathlib.Path) -> bool:
        try:
            child.relative_to(parent)
            return True
        except ValueError:
            return False

    # A Landlock rule cannot subtract a nested deny from an allowed parent.
    # Refuse such a configuration instead of claiming the sealed path is safe.
    if any(contains(allowed, secret) for allowed in reads + writes for secret in denied):
        return None

    system_roots = ["/bin", "/dev", "/lib", "/lib64", "/sbin", "/sys", "/usr"]
    for raw in system_roots:
        path = pathlib.Path(raw)
        if path.is_dir():
            try:
                reads.append(path.resolve(strict=True))
            except (OSError, RuntimeError):
                return None
    # Keep configuration access to the exact files needed for identity,
    # resolver, and TLS setup.  Never allow the whole /etc tree: that would
    # expose controller-owned configuration such as /etc/codex.
    for raw in (
        "/etc/ld.so.cache",
        "/etc/nsswitch.conf",
        "/etc/passwd",
        "/etc/group",
        "/etc/hosts",
        "/etc/resolv.conf",
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/ssl/openssl.cnf",
        "/etc/localtime",
    ):
        path = pathlib.Path(raw)
        try:
            resolved = path.resolve(strict=True)
        except (OSError, RuntimeError):
            return None
        if not resolved.is_file():
            return None
        reads.extend((path, resolved))
    # Linux resolves /etc/resolv.conf through a symlink into /run.  The
    # controller must allow only that exact target, rather than opening the
    # whole /run tree.  The private CODEX_HOME and --ignore-user-config path
    # must not grant access to the host user's config.
    try:
        resolver_target = pathlib.Path("/etc/resolv.conf").resolve(strict=True)
    except (OSError, RuntimeError):
        return None
    if not resolver_target.is_file():
        return None
    reads.append(resolver_target)
    if any(contains(allowed, secret) for allowed in reads for secret in denied):
        return None
    for executable in executables:
        reads.append(executable.parent)
        # A symlink-resolved executable may live outside its command's parent.
    preload_lib = _linux_preload_lib_path()
    if preload_lib is None:
        return None
    reads.append(preload_lib.parent)
    writes.append(pathlib.Path("/dev/null"))

    # Worktrees may use a .git file that points at an admin directory outside
    # the checkout. Allow that exact metadata tree so normal git inspection
    # remains available without granting the checkout's parent directory.
    for root in list(reads):
        marker = root / ".git"
        try:
            if marker.is_file():
                text = marker.read_text(encoding="utf-8").strip()
                if text.startswith("gitdir:"):
                    gitdir = pathlib.Path(text.split(":", 1)[1].strip())
                    if not gitdir.is_absolute():
                        gitdir = marker.parent / gitdir
                    gitdir = gitdir.resolve(strict=True)
                    reads.append(gitdir)
                    common = gitdir / "commondir"
                    if common.is_file():
                        common_path = pathlib.Path(common.read_text(encoding="utf-8").strip())
                        if not common_path.is_absolute():
                            common_path = gitdir / common_path
                        reads.append(common_path.resolve(strict=True))
        except (OSError, RuntimeError, ValueError):
            return None
    if any(contains(allowed, secret) for allowed in reads + writes for secret in denied):
        return None

    # Keep the runtime and target out of any accidental duplicate path list;
    # deterministic argv helps audit logs and tests.
    read_unique = sorted(set(reads), key=str)
    write_unique = sorted(set(writes), key=str)
    launcher_fd = _open_verified_launcher(launcher)
    if launcher_fd is None:
        return None
    try:
        launcher_args: list[str] = [f"/proc/self/fd/{launcher_fd}"]
        for path in read_unique:
            launcher_args.extend(("--read", str(path)))
        for path in write_unique:
            launcher_args.extend(("--write", str(path)))
        launcher_args.append("--")

        # Retain preload containment as defense-in-depth.  Landlock remains the
        # kernel boundary and is what protects static binaries/raw syscalls.
        preload = _linux_sandbox_prefix(denied)
        if preload is None:
            os.close(launcher_fd)
            return None
        return _PinnedLauncherCommand(preload + launcher_args, launcher_fd)
    except Exception:
        os.close(launcher_fd)
        raise


_darwin_sandbox_exec_verified: "Optional[bool]" = None


def _verify_darwin_sandbox_exec() -> bool:
    """Test whether sandbox-exec can apply profiles on this macOS host.

    When executing inside an existing sandbox (e.g., Antigravity agent CLI),
    sandbox-exec fails with code 71 (`sandbox-exec: sandbox_apply: Operation not permitted`).
    This canary check runs a minimal sandbox profile and caches the result for the
    process lifetime.
    """
    global _darwin_sandbox_exec_verified
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
        profile = _build_sandbox_profile(sealed_docs)
        return [sandbox_exec, "-p", profile] + args
    if sys.platform.startswith("linux"):
        sealed_docs = _sealed_benchmark_doc_paths(workdir)
        prefix = _linux_sandbox_prefix(_holdout_denied_paths() + sealed_docs)
        if prefix is None:
            return None
        return prefix + args
    return None
