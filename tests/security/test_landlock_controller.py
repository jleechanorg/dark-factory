"""Kernel-enforced isolation for the Linux controller reviewer transport.

These tests deliberately exercise the bypasses that an ``LD_PRELOAD`` shim
cannot cover: a direct ``openat(2)`` syscall and a statically linked binary.
The controller transport must deny both while still allowing the reviewed
worktree and its private Codex runtime.
"""

from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import threading

import pytest

from runner.handler_dispatch import _build_controller_codex_transport
from runner.handler_sandbox import (
    _extend_pinned_launcher_command,
    _holdout_denied_paths,
    _linux_codex_runtime_paths,
    _linux_controller_sandbox_prefix,
    _linux_landlock_launcher_path,
    _open_verified_launcher,
    _reset_linux_landlock_launcher_cache_for_tests,
)

linux_only = pytest.mark.skipif(
    not sys.platform.startswith("linux"), reason="Landlock is Linux-only"
)


def _native_codex_package_name() -> str:
    machine = os.uname().machine
    package = {
        "x86_64": "codex-linux-x64",
        "amd64": "codex-linux-x64",
        "aarch64": "codex-linux-arm64",
        "arm64": "codex-linux-arm64",
    }.get(machine)
    if package is None:
        pytest.skip(f"unsupported Linux architecture: {machine}")
    return package


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
def test_pinned_launcher_survives_cached_target_replacement(tmp_path, monkeypatch):
    import runner.handler_sandbox as sandbox

    cache_dir = pathlib.Path(tempfile.mkdtemp(prefix="df-landlock-pinned-", dir=pathlib.Path.home()))
    monkeypatch.setattr(sandbox, "_linux_landlock_cache_dir", lambda: cache_dir)
    sandbox._reset_linux_landlock_launcher_cache_for_tests()
    fd = None
    try:
        launcher = sandbox._linux_landlock_launcher_path()
        assert launcher is not None
        fd = _open_verified_launcher(launcher)
        assert fd is not None
        fake = tmp_path / "replacement"
        fake.write_text("#!/bin/sh\necho REPLACEMENT-RAN\n", encoding="utf-8")
        fake.chmod(0o700)
        replaced = threading.Event()

        def replace_target():
            os.replace(fake, launcher)
            replaced.set()

        replacer = threading.Thread(target=replace_target)
        replacer.start()
        replacer.join()
        assert replaced.is_set()

        proc = subprocess.run(
            [
                f"/proc/self/fd/{fd}",
                "--read",
                str(tmp_path),
                "--read",
                "/usr",
                "--write",
                "/dev/null",
                "--",
                "/bin/true",
            ],
            pass_fds=(fd,),
            capture_output=True,
            text=True,
            check=False,
        )
        assert proc.returncode == 0, proc.stderr
        assert "REPLACEMENT-RAN" not in proc.stdout
    finally:
        if fd is not None:
            os.close(fd)
        sandbox._reset_linux_landlock_launcher_cache_for_tests()
        shutil.rmtree(cache_dir, ignore_errors=True)


@linux_only
def test_launcher_rejects_replacement_before_pinning(tmp_path, monkeypatch):
    import runner.handler_sandbox as sandbox

    cache_dir = pathlib.Path(tempfile.mkdtemp(prefix="df-landlock-publish-", dir=pathlib.Path.home()))
    monkeypatch.setattr(sandbox, "_linux_landlock_cache_dir", lambda: cache_dir)
    sandbox._reset_linux_landlock_launcher_cache_for_tests()
    try:
        launcher = sandbox._linux_landlock_launcher_path()
        assert launcher is not None
        fake = tmp_path / "replacement"
        fake.write_text("#!/bin/sh\necho REPLACEMENT-RAN\n", encoding="utf-8")
        fake.chmod(0o700)
        ready = threading.Barrier(2)

        def replace_target():
            ready.wait()
            os.replace(fake, launcher)

        replacer = threading.Thread(target=replace_target)
        replacer.start()
        ready.wait()
        replacer.join()
        assert _open_verified_launcher(launcher) is None
    finally:
        sandbox._reset_linux_landlock_launcher_cache_for_tests()
        shutil.rmtree(cache_dir, ignore_errors=True)


@linux_only
def test_landlock_prefix_closes_pinned_fd_when_preload_fails(tmp_path, monkeypatch):
    import runner.handler_sandbox as sandbox

    fd = os.open(sandbox._LINUX_LANDLOCK_SOURCE, os.O_RDONLY)
    allowed = tmp_path / "allowed"
    allowed.mkdir()
    sealed = tmp_path / "sealed"
    sealed.mkdir()
    monkeypatch.setattr(sandbox, "_linux_landlock_launcher_path", lambda: sandbox._LINUX_LANDLOCK_SOURCE)
    monkeypatch.setattr(sandbox, "_open_verified_launcher", lambda path: fd)
    monkeypatch.setattr(sandbox, "_linux_sandbox_prefix", lambda denied: None)
    try:
        assert sandbox._linux_controller_sandbox_prefix(
            denied_paths=[sealed],
            read_paths=[allowed],
            writable_paths=[],
        ) is None
        with pytest.raises(OSError):
            os.fstat(fd)
        fd = None
    finally:
        if fd is not None:
            os.close(fd)


@linux_only
def test_landlock_prefix_does_not_allow_proc_root(tmp_path, monkeypatch):
    """The controller allow-list must not grant the host process tree."""
    import runner.handler_sandbox as sandbox

    allowed = tmp_path / "allowed"
    allowed.mkdir()
    fd = os.open(__file__, os.O_RDONLY)
    monkeypatch.setattr(sandbox, "_linux_landlock_launcher_path", lambda: __file__)
    monkeypatch.setattr(sandbox, "_open_verified_launcher", lambda _path: fd)
    monkeypatch.setattr(sandbox, "_linux_preload_lib_path", lambda: pathlib.Path(__file__))
    monkeypatch.setattr(sandbox, "_linux_sandbox_prefix", lambda _denied: ["env"])
    try:
        prefix = _linux_controller_sandbox_prefix(
            denied_paths=[tmp_path / "sealed"],
            read_paths=[allowed],
            writable_paths=[],
        )
        assert prefix is not None
        read_paths = [
            pathlib.Path(prefix[index + 1])
            for index, value in enumerate(prefix)
            if value == "--read"
        ]
        assert pathlib.Path("/proc") not in read_paths
        assert not any(path.is_relative_to("/proc") for path in read_paths)
    finally:
        os.close(fd)


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
def test_codex_js_launcher_resolves_hoisted_native_runtime_root(tmp_path):
    """npm hoisting puts the platform package beside the JS package."""
    native_package = _native_codex_package_name()
    package = tmp_path / "node_modules" / "@openai" / "codex"
    launcher = package / "bin" / "codex.js"
    launcher.parent.mkdir(parents=True)
    launcher.write_text("#!/usr/bin/env node\n", encoding="utf-8")
    native = (
        tmp_path
        / "node_modules"
        / "@openai"
        / native_package
        / "vendor"
        / "x86_64-unknown-linux-musl"
        / "bin"
        / "codex"
    )
    native.parent.mkdir(parents=True)
    native.write_bytes(b"native")
    paths = _linux_codex_runtime_paths(launcher)
    assert paths is not None
    assert native.parents[3].resolve() in paths


@linux_only
def test_codex_js_launcher_rejects_mutable_native_runtime_root(tmp_path):
    """A world-writable native package must never enter the allow-list."""
    native_package = _native_codex_package_name()
    package = tmp_path / "node_modules" / "@openai" / "codex"
    launcher = package / "bin" / "codex.js"
    launcher.parent.mkdir(parents=True)
    launcher.write_text("#!/usr/bin/env node\n", encoding="utf-8")
    native_root = tmp_path / "node_modules" / "@openai" / native_package
    native_root.mkdir(parents=True)
    native_root.chmod(0o777)
    try:
        assert _linux_codex_runtime_paths(launcher) is None
    finally:
        native_root.chmod(0o755)


@linux_only
def test_controller_transport_uses_declared_real_codex_with_private_home(
    tmp_path, monkeypatch
):
    """A PATH wrapper must not resolve its real Codex through private HOME."""
    wrapper_dir = tmp_path / "bin"
    wrapper_dir.mkdir()
    wrapper = wrapper_dir / "codex"
    real_codex = tmp_path / "libexec" / "codex"
    real_codex.parent.mkdir()
    real_codex.write_text(
        "#!/bin/sh\n"
        "printf REAL-CODEX-RAN\\n\n",
        encoding="utf-8",
    )
    real_codex.chmod(0o755)
    wrapper.write_text(
        "#!/bin/sh\n"
        f"REAL_BIN={real_codex}\n"
        "exec \"$HOME/.local/libexec/codex\" \"$@\"\n",
        encoding="utf-8",
    )
    wrapper.chmod(0o755)
    private_home = tmp_path / "private-home"
    private_home.mkdir()
    sealed = tmp_path / "sealed"
    sealed.mkdir()
    monkeypatch.setenv("PATH", f"{wrapper_dir}:/usr/bin:/bin")
    monkeypatch.setattr(
        "runner.handlers._linux_controller_sandbox_prefix",
        lambda **kwargs: ["/usr/bin/env", f"DENY_PATHS={sealed}"],
    )

    transport = _build_controller_codex_transport(
        ["/usr/bin/env", f"DENY_PATHS={sealed}", "codex", "exec", "--yolo", "PROMPT"],
        read_only_path=tmp_path,
    )
    proc = subprocess.run(
        transport,
        cwd=tmp_path,
        env={"HOME": str(private_home), "PATH": f"{wrapper_dir}:/usr/bin:/bin"},
        input="{}\n",
        capture_output=True,
        text=True,
        check=False,
    )

    # The current transport executes bare ``codex`` and therefore runs the
    # wrapper under private HOME, which exits 127 on Linux (126 on macOS's
    # shell).  It must instead invoke this trusted target.
    assert proc.returncode == 0, proc.stderr
    assert "REAL-CODEX-RAN" in proc.stdout
    assert str(real_codex) in transport


@linux_only
def test_controller_transport_allows_real_codex_js_interpreter_from_service_path(
    tmp_path, monkeypatch
):
    """Landlock must allow the env-resolved Node interpreter of REAL_BIN."""
    wrapper_dir = tmp_path / "local-bin"
    wrapper_dir.mkdir()
    node_dir = tmp_path / "nvm" / "versions" / "node" / "v22.22.0" / "bin"
    node_dir.mkdir(parents=True)
    node = node_dir / "node"
    node.write_text(
        "#!/bin/sh\n"
        "printf NODE-RUNTIME-RAN\\n\n",
        encoding="utf-8",
    )
    node.chmod(0o755)
    real_codex = (
        tmp_path
        / "npm-global"
        / "lib"
        / "node_modules"
        / "@openai"
        / "codex"
        / "bin"
        / "codex.js"
    )
    real_codex.parent.mkdir(parents=True)
    real_codex.write_text(
        "#!/usr/bin/env node\n"
        "// controller entrypoint\n",
        encoding="utf-8",
    )
    real_codex.chmod(0o755)
    wrapper = wrapper_dir / "codex"
    wrapper.write_text(
        "#!/bin/sh\n"
        f"REAL_BIN={real_codex}\n"
        "exec \"$HOME/.local/libexec/codex\" \"$@\"\n",
        encoding="utf-8",
    )
    wrapper.chmod(0o755)
    private_home = tmp_path / "private-home"
    private_home.mkdir()
    sealed = tmp_path / "sealed"
    sealed.mkdir()
    service_path = f"{wrapper_dir}:{node_dir}:/usr/bin:/bin"
    monkeypatch.setenv("PATH", service_path)
    observed: dict[str, object] = {}

    def fake_landlock(**kwargs):
        observed.update(kwargs)
        return ["/usr/bin/env", f"DENY_PATHS={sealed}"]

    monkeypatch.setattr(
        "runner.handlers._linux_controller_sandbox_prefix", fake_landlock
    )
    transport = _build_controller_codex_transport(
        ["/usr/bin/env", f"DENY_PATHS={sealed}", "codex", "exec", "--yolo", "PROMPT"],
        read_only_path=tmp_path,
    )
    proc = subprocess.run(
        transport,
        cwd=tmp_path,
        env={"HOME": str(private_home), "PATH": service_path},
        input="{}\n",
        capture_output=True,
        text=True,
        check=False,
    )

    assert proc.returncode == 0, proc.stderr
    assert "NODE-RUNTIME-RAN" in proc.stdout
    read_paths = observed["read_paths"]
    assert isinstance(read_paths, list)
    assert node_dir.resolve() in read_paths


@linux_only
def test_controller_transport_accepts_native_real_codex_without_interpreter(
    tmp_path, monkeypatch
):
    """A native REAL_BIN must not require text-readable interpreter metadata."""
    wrapper_dir = tmp_path / "local-bin"
    wrapper_dir.mkdir()
    wrapper = wrapper_dir / "codex"
    wrapper.write_text(
        "#!/bin/sh\n"
        "REAL_BIN=/usr/bin/true\n"
        "exec \"$HOME/.local/libexec/codex\" \"$@\"\n",
        encoding="utf-8",
    )
    wrapper.chmod(0o755)
    private_home = tmp_path / "private-home"
    private_home.mkdir()
    sealed = tmp_path / "sealed"
    sealed.mkdir()
    service_path = f"{wrapper_dir}:/usr/bin:/bin"
    monkeypatch.setenv("PATH", service_path)
    observed: dict[str, object] = {}

    def fake_landlock(**kwargs):
        observed.update(kwargs)
        return ["/usr/bin/env", f"DENY_PATHS={sealed}"]

    monkeypatch.setattr(
        "runner.handlers._linux_controller_sandbox_prefix", fake_landlock
    )
    transport = _build_controller_codex_transport(
        ["/usr/bin/env", f"DENY_PATHS={sealed}", "codex", "exec", "--yolo", "PROMPT"],
        read_only_path=tmp_path,
    )
    proc = subprocess.run(
        transport,
        cwd=tmp_path,
        env={"HOME": str(private_home), "PATH": service_path},
        input="{}\n",
        capture_output=True,
        text=True,
        check=False,
    )

    assert proc.returncode == 0, proc.stderr
    assert str(pathlib.Path("/usr/bin/true")) in transport
    read_paths = observed["read_paths"]
    assert isinstance(read_paths, list)
    assert pathlib.Path("/usr/bin") in read_paths


@linux_only
def test_codex_runtime_allows_private_primary_group_directory(tmp_path, monkeypatch):
    from types import SimpleNamespace

    import runner.handler_sandbox as sandbox

    package = tmp_path / "node_modules" / "@openai" / "codex"
    launcher = package / "bin" / "codex.js"
    launcher.parent.mkdir(parents=True)
    launcher.write_text("#!/usr/bin/env node\n", encoding="utf-8")
    package.chmod(0o770)
    username = "controller-user"
    gid = os.getgid()
    monkeypatch.setattr(sandbox.pwd, "getpwuid", lambda uid: SimpleNamespace(pw_name=username))
    monkeypatch.setattr(
        sandbox.pwd,
        "getpwall",
        lambda: [SimpleNamespace(pw_uid=os.getuid(), pw_gid=gid, pw_name=username)],
    )
    monkeypatch.setattr(
        sandbox.grp,
        "getgrall",
        lambda: [SimpleNamespace(gr_gid=gid, gr_mem=[username])],
    )
    paths = _linux_codex_runtime_paths(launcher)
    assert paths is not None


@linux_only
def test_codex_runtime_rejects_extra_primary_group_member_or_gid(tmp_path, monkeypatch):
    from types import SimpleNamespace

    import runner.handler_sandbox as sandbox

    package = tmp_path / "node_modules" / "@openai" / "codex"
    launcher = package / "bin" / "codex.js"
    launcher.parent.mkdir(parents=True)
    launcher.write_text("#!/usr/bin/env node\n", encoding="utf-8")
    package.chmod(0o770)
    username = "controller-user"
    gid = os.getgid()
    monkeypatch.setattr(sandbox.pwd, "getpwuid", lambda uid: SimpleNamespace(pw_name=username))
    monkeypatch.setattr(
        sandbox.pwd,
        "getpwall",
        lambda: [SimpleNamespace(pw_uid=os.getuid(), pw_gid=gid, pw_name=username)],
    )
    monkeypatch.setattr(
        sandbox.grp,
        "getgrall",
        lambda: [SimpleNamespace(gr_gid=gid, gr_mem=[username, "other-user"])],
    )
    assert _linux_codex_runtime_paths(launcher) is None

    monkeypatch.setattr(sandbox.os, "getgid", lambda: gid + 1)
    assert _linux_codex_runtime_paths(launcher) is None


@linux_only
def test_landlock_prefix_keeps_missing_canonical_default_denial(tmp_path, monkeypatch):
    import runner.handler_sandbox as sandbox

    fake_home = tmp_path / "fake-home"
    explicit = tmp_path / "explicit-holdouts"
    allowed = tmp_path / "allowed"
    fake_home.mkdir()
    explicit.mkdir()
    allowed.mkdir()
    monkeypatch.setattr(pathlib.Path, "home", lambda: fake_home)
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(explicit))
    denied = _holdout_denied_paths()
    default = fake_home / "projects" / "dark-factory-holdouts"
    assert default in denied
    assert not default.is_dir()

    captured: list[pathlib.Path] = []
    fd = os.open(__file__, os.O_RDONLY)
    monkeypatch.setattr(sandbox, "_linux_landlock_launcher_path", lambda: __file__)
    monkeypatch.setattr(sandbox, "_open_verified_launcher", lambda _path: fd)

    def fake_preload(paths):
        captured.extend(paths)
        return ["/usr/bin/env", f"DENY_PATHS={':'.join(map(str, paths))}"]

    monkeypatch.setattr(sandbox, "_linux_sandbox_prefix", fake_preload)
    try:
        prefix = _linux_controller_sandbox_prefix(
            denied_paths=denied,
            read_paths=[allowed],
            writable_paths=[],
        )
        assert prefix is not None
        assert set(denied).issubset(captured)
        deny_arg = next(arg for arg in prefix if arg.startswith("DENY_PATHS="))
        assert all(str(path) in deny_arg for path in denied)
    finally:
        os.close(fd)


@linux_only
def test_landlock_denies_raw_openat_from_static_binary(tmp_path, monkeypatch):
    launcher = _linux_landlock_launcher_path()
    assert launcher is not None
    raw_open = _compile_static_raw_open(tmp_path / "tool")
    allowed = tmp_path / "allowed"
    denied = tmp_path / "sealed"
    fake_home = tmp_path / "fake-home"
    allowed.mkdir()
    denied.mkdir()
    fake_home.mkdir()
    monkeypatch.setattr(pathlib.Path, "home", lambda: fake_home)
    monkeypatch.setenv("DARK_FACTORY_HOLDOUTS", str(denied))
    default = fake_home / "projects" / "dark-factory-holdouts"
    assert default in _holdout_denied_paths()
    assert not default.is_dir()

    prefix = _linux_controller_sandbox_prefix(
        denied_paths=_holdout_denied_paths(),
        read_paths=[allowed],
        writable_paths=[],
        executable_paths=[raw_open],
    )
    assert prefix is not None

    default.mkdir(parents=True)
    secret = default / "scenario.txt"
    secret.write_text("STATIC-RAW-SYSCALL-SECRET", encoding="utf-8")
    proc = subprocess.run(
        _extend_pinned_launcher_command(prefix, [str(raw_open), str(secret)]),
        pass_fds=prefix.pass_fds,
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
def test_landlock_denies_unlisted_etc_sibling_from_static_binary(tmp_path):
    """Exact runtime files must not expand into a readable /etc directory."""
    launcher = _linux_landlock_launcher_path()
    assert launcher is not None
    raw_open = _compile_static_raw_open(tmp_path / "tool")
    work = tmp_path / "work"
    work.mkdir()

    denied = pathlib.Path("/etc/hostname")
    if not denied.is_file():
        pytest.skip(f"unlisted /etc sibling unavailable: {denied}")
    prefix = _linux_controller_sandbox_prefix(
        denied_paths=[denied],
        read_paths=[work],
        writable_paths=[],
        executable_paths=[raw_open],
    )
    assert prefix is not None
    proc = subprocess.run(
        _extend_pinned_launcher_command(prefix, [str(raw_open), str(denied)]),
        pass_fds=prefix.pass_fds,
        capture_output=True,
        text=True,
        check=False,
    )
    prefix.close_launcher()
    assert proc.returncode == 3, proc.stderr
    assert "Permission denied" not in proc.stdout


@linux_only
def test_landlock_denies_unrelated_proc_cmdline_from_static_binary(tmp_path):
    """The controller must not read another process through the host /proc."""
    launcher = _linux_landlock_launcher_path()
    assert launcher is not None
    proc_cmdline = pathlib.Path("/proc/1/cmdline")
    if not proc_cmdline.is_file():
        pytest.skip(f"unrelated process proc entry unavailable: {proc_cmdline}")
    raw_open = _compile_static_raw_open(tmp_path / "tool")
    work = tmp_path / "work"
    work.mkdir()

    prefix = _linux_controller_sandbox_prefix(
        denied_paths=[tmp_path / "sealed"],
        read_paths=[work],
        writable_paths=[],
        executable_paths=[raw_open],
    )
    assert prefix is not None
    try:
        result = subprocess.run(
            _extend_pinned_launcher_command(prefix, [str(raw_open), str(proc_cmdline)]),
            pass_fds=prefix.pass_fds,
            capture_output=True,
            text=True,
            check=False,
        )
    finally:
        prefix.close_launcher()
    assert result.returncode == 3, result.stderr
    assert "Permission denied" not in result.stdout


@linux_only
def test_landlock_allows_resolver_symlink_target_but_not_run_sibling(tmp_path):
    """The resolver symlink target is an exact read rule, not a /run grant."""
    launcher = _linux_landlock_launcher_path()
    assert launcher is not None
    resolver = pathlib.Path("/etc/resolv.conf")
    target = resolver.resolve(strict=True)
    sibling = target.parent / ("resolv.conf" if target.name != "resolv.conf" else "stub-resolv.conf")
    if not sibling.is_file():
        pytest.skip(f"resolver sibling unavailable: {sibling}")
    work = tmp_path / "work"
    work.mkdir()
    sealed = tmp_path / "sealed"
    sealed.mkdir()
    raw_open = _compile_static_raw_open(work / "tool")

    prefix = _linux_controller_sandbox_prefix(
        denied_paths=[sealed],
        read_paths=[work],
        writable_paths=[],
        executable_paths=[raw_open],
    )
    assert prefix is not None
    allowed = subprocess.run(
        _extend_pinned_launcher_command(prefix, [str(raw_open), str(resolver)]),
        pass_fds=prefix.pass_fds,
        capture_output=True,
        text=True,
        check=False,
    )
    assert allowed.returncode == 0, allowed.stderr
    prefix.close_launcher()

    prefix = _linux_controller_sandbox_prefix(
        denied_paths=[sealed],
        read_paths=[work],
        writable_paths=[],
        executable_paths=[raw_open],
    )
    assert prefix is not None
    denied = subprocess.run(
        _extend_pinned_launcher_command(prefix, [str(raw_open), str(sibling)]),
        pass_fds=prefix.pass_fds,
        capture_output=True,
        text=True,
        check=False,
    )
    prefix.close_launcher()
    assert denied.returncode == 3, denied.stderr


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
        pass_fds=transport.pass_fds,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert "VISIBLE-REPO-FILE" in proc.stdout
    assert "CONTROLLER-SECRET" not in proc.stdout
    assert (runtime / "runtime-marker").read_text(encoding="utf-8") == "runtime"
