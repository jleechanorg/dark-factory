"""File-disjoint shim for jleechan-c5q.

When ``agy`` is not on PATH, ``subprocess.Popen(["agy", ...])`` raises
``FileNotFoundError`` which propagates up and panics the codergen node.
``runner/handlers.py`` is currently in-flight (WIP branch
``feat/agento-dark-factory-implement-attractor-runner-parity-bead``)
and cannot be modified, so this module monkey-patches
``subprocess.Popen`` to return a stub when the first arg is ``"agy"``.

The stub mimics the surface area of ``subprocess.Popen`` that
``_codergen`` actually uses (``communicate``, ``returncode``, ``args``,
``stdout``, ``stderr``, ``pid``, ``kill``, ``wait``, ``poll``) and
returns a clean result with exit 127 and a ``backend_missing=true``
marker, which the post-Popen code converts into a normal
``Result(outcome="failure", output=..., metadata=...)``.

Import this from ``bin/dark-factory`` (and ``bin/df-healer``) BEFORE
the runner starts so the patch is active for every ``Popen`` call.
``handlers.py`` and ``__main__.py`` are not modified.

This shim is temporary. Once the WIP branch lands the upstream
``try/except FileNotFoundError`` in ``handlers.py`` and is merged, this
module can be retired.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from typing import Any, Sequence

_BACKEND_MARKER = "backend_missing=true\n"
_BACKEND_RC = 127


class _MissingBackendPopen:
    """Stub Popen returned when ``agy`` is configured but missing from PATH.

    Mimics the surface area of ``subprocess.Popen`` that
    ``runner/handlers._codergen`` actually uses (communicate, returncode,
    args, stdout, stderr, kill, wait, poll, pid). The handlers code reads
    these attributes to build a ``Result``; we just need them to be sane.
    """

    def __init__(self, args: Sequence[Any], **kwargs: Any) -> None:
        self.args = args
        self.stdin = kwargs.get("stdin")
        self.stdout: str = _BACKEND_MARKER
        self.stderr: str = ""
        self.pid: int = -1
        self.returncode: int | None = _BACKEND_RC

    def communicate(self, timeout: float | None = None) -> tuple[str, str]:
        return (self.stdout, self.stderr)

    def kill(self) -> None:
        pass

    def wait(self, timeout: float | None = None) -> int:
        return self.returncode if self.returncode is not None else 0

    def poll(self) -> int | None:
        return self.returncode


_safe_popen_installed = False
_real_popen = subprocess.Popen
_safe_popen = _real_popen  # overwritten by install() if agy is missing


def install() -> bool:
    """Install the agy-safe Popen patch if agy is missing from PATH.

    Returns:
        True if the patch was installed (or was already installed).
        False if agy is on PATH (patch not needed).

    Idempotent. Safe to call from ``bin/dark-factory`` on every
    invocation; a no-op when agy is available.
    """
    global _safe_popen_installed, _safe_popen
    if _safe_popen_installed:
        return True
    if shutil.which("agy"):
        return False

    def _safe_popen_impl(args: Any, *popen_args: Any, **kwargs: Any) -> Any:
        first = args[0] if isinstance(args, (list, tuple)) and args else None
        if first == "agy":
            return _MissingBackendPopen(args, **kwargs)
        return _real_popen(args, *popen_args, **kwargs)

    _safe_popen = _safe_popen_impl
    subprocess.Popen = _safe_popen  # type: ignore[assignment]
    _safe_popen_installed = True
    return True


def main(argv: list[str] | None = None) -> int:
    argv = argv if argv is not None else sys.argv[1:]
    installed = install()
    print(f"agy_safe: patch_installed={installed}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
