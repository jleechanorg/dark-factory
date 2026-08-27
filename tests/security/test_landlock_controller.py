"""Kernel-enforced isolation for the Linux controller reviewer transport.

These tests deliberately exercise the bypasses that an ``LD_PRELOAD`` shim
cannot cover: a direct ``openat(2)`` syscall and a statically linked binary.
The controller transport must deny both while still allowing the reviewed
worktree and its private Codex runtime.
"""

from __future__ import annotations

import pathlib
import hashlib
import shutil
import subprocess
import sys
import tempfile

import pytest

from runner.handler_dispatch import _build_controller_codex_transport
from runner.handler_sandbox import (
    _linux_controller_sandbox_prefix,
    _linux_codex_runtime_paths,
    _linux_landlock_launcher_path,
    _reset_linux_landlock_launcher_cache_for_tests,
)


linux_only = pytest.mark.skipif(
    not sys.platform.startswith("linux"), reason="Landlock is Linux-only"
)


RAW_OPEN_SOURCE = r'''
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    int fd = syscall(SYS_openat, AT_FDCWD, argv[1], O_RDONLY, 0);
    if (fd < 0) return 3;
    char buf[256];
    ssize_t n = read(fd, buf, sizeof(buf));
    close(fd);
    if (n <= 0) return 4;
    if (write(STDOUT_FILENO, buf, (size_t)n) != n) return 5;
    return 0;
}
'''


def _compile_static_raw_open(tmp_path: pathlib.Path) -> pathlib.Path:
    compiler = shutil.which("cc") or shutil.which("gcc")
    if compiler is None:
        pytest.skip("C compiler unavailable for static raw-syscall probe")
    source = tmp_path / "raw_open.c"
    binary = tmp_path / "raw_open"
    tmp_path.mkdir(parents=True, exist_ok=True)
    source.write_text(RAW_OPEN_SOURCE, encoding="utf-8")
    proc = subprocess.run(
        [compiler, "-static", "-O2", "-o", str(binary), str(source)],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        pytest.skip(f"static C toolchain unavailable: {proc.stderr.strip()}")
    return binary


@linux_only
def test_landlock_launcher_builds_and_reports_kernel_support():
    launcher = _linux_landlock_launcher_path()
    assert launcher is not None
    assert launcher.is_file()
    assert launcher.stat().st_mode & 0o111


@linux_only
@pytest.mark.parametrize("preseed_kind", ["regular", "symlink"])
def test_landlock_launcher_replaces_untrusted_cached_entry(tmp_path, monkeypatch, preseed_kind):
    import runner.handler_sandbox as sandbox

    cache_dir = pathlib.Path(tempfile.mkdtemp(prefix="df-landlock-cache-", dir=pathlib.Path.home()))
    source_digest = hashlib.sha256(sandbox._LINUX_LANDLOCK_SOURCE.read_bytes()).hexdigest()
    target = cache_dir / f"landlock-launcher-{source_digest[:16]}"
    fake = tmp_path / "fake-launcher"
    fake.write_text("#!/bin/sh\necho PRESEEDED\n", encoding="utf-8")
    fake.chmod(0o700)
    if preseed_kind == "symlink":
        target.symlink_to(fake)
    else:
        target.write_bytes(fake.read_bytes())
        target.chmod(0o700)

    monkeypatch.setattr(sandbox, "_linux_landlock_cache_dir", lambda: cache_dir)
    sandbox._reset_linux_landlock_launcher_cache_for_tests()
    try:
        launcher = sandbox._linux_landlock_launcher_path()
        assert launcher == target
        assert target.is_file() and not target.is_symlink()
        assert target.read_bytes() != fake.read_bytes()
        manifest = target.with_name(target.name + ".manifest")
        assert manifest.is_file()
        assert f"source_sha256={source_digest}" in manifest.read_text(encoding="ascii")
    finally:
        sandbox._reset_linux_landlock_launcher_cache_for_tests()
        shutil.rmtree(cache_dir, ignore_errors=True)


@linux_only
def test_landlock_requires_abi_three_for_truncate_enforcement(monkeypatch):
    import runner.handler_sandbox as sandbox

    monkeypatch.setattr(sandbox, "_linux_landlock_abi", lambda: 2)
    _reset_linux_landlock_launcher_cache_for_tests()
    try:
        assert _linux_landlock_launcher_path() is None
    finally:
        _reset_linux_landlock_launcher_cache_for_tests()


@linux_only
def test_codex_js_launcher_allows_bundled_native_runtime_root(tmp_path):
    package = tmp_path / "node_modules" / "@openai" / "codex"
    launcher = package / "bin" / "codex.js"
    launcher.parent.mkdir(parents=True)
    launcher.write_text("#!/usr/bin/env node\n", encoding="utf-8")
    native = package / "node_modules" / "@openai" / "codex-linux-x64" / "vendor" / "bin" / "codex"
    native.parent.mkdir(parents=True)
    native.write_bytes(b"native")
    paths = _linux_codex_runtime_paths(launcher)
    assert paths is not None
    assert package.resolve() in paths
    assert native.parent.resolve().is_relative_to(package.resolve())


@linux_only
def test_landlock_denies_raw_openat_from_static_binary(tmp_path):
    launcher = _linux_landlock_launcher_path()
    assert launcher is not None
    raw_open = _compile_static_raw_open(tmp_path / "tool")
    allowed = tmp_path / "allowed"
    denied = tmp_path / "sealed"
    allowed.mkdir()
    denied.mkdir()
    secret = denied / "scenario.txt"
    secret.write_text("STATIC-RAW-SYSCALL-SECRET", encoding="utf-8")

    prefix = _linux_controller_sandbox_prefix(
        denied_paths=[denied],
        read_paths=[allowed],
        writable_paths=[],
        executable_paths=[raw_open],
    )
    assert prefix is not None
    proc = subprocess.run(
        prefix + [str(raw_open), str(secret)],
        capture_output=True,
        text=True,
        check=False,
    )
    # The probe must execute and report the raw openat denial itself.  A 127
    # from the launcher would only prove that the executable was not allowed.
    assert proc.returncode == 3, proc.stderr
    assert "Permission denied" not in proc.stdout
    assert "STATIC-RAW-SYSCALL-SECRET" not in proc.stdout


@linux_only
def test_controller_transport_denies_secret_but_allows_repo_and_runtime(tmp_path):
    workdir = tmp_path / "repo"
    workdir.mkdir()
    runtime = tmp_path / "runtime"
    runtime.mkdir()
    sealed = tmp_path / "sealed"
    sealed.mkdir()
    secret = sealed / "scenario.txt"
    secret.write_text("CONTROLLER-SECRET", encoding="utf-8")
    marker = workdir / "visible.txt"
    marker.write_text("VISIBLE-REPO-FILE", encoding="utf-8")
    codex = workdir / "codex"
    codex.write_text(
        "#!/bin/sh\n"
        "cat visible.txt\n"
        "printf runtime > \"$HOME/runtime-marker\"\n"
        "cat \"$TEST_SECRET\" 2>/dev/null || true\n",
        encoding="utf-8",
    )
    codex.chmod(0o755)

    sandboxed = _linux_controller_sandbox_prefix(
        denied_paths=[sealed],
        read_paths=[workdir],
        writable_paths=[runtime],
    )
    assert sandboxed is not None
    transport = _build_controller_codex_transport(
        sandboxed + [str(codex), "exec", "ignored"],
        read_only_path=workdir,
        writable_path=runtime,
    )
    proc = subprocess.run(
        transport,
        cwd=workdir,
        env={"HOME": str(runtime), "TEST_SECRET": str(secret), "PATH": "/usr/bin:/bin"},
        input="{}\n",
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert "VISIBLE-REPO-FILE" in proc.stdout
    assert "CONTROLLER-SECRET" not in proc.stdout
    assert (runtime / "runtime-marker").read_text(encoding="utf-8") == "runtime"
