"""Unit tests for the shared git-lfs post-hook helper.

Contract (issue jleechanorg/dark-factory#266):
  - `.githooks/_git-lfs-hook.sh` exists and is executable.
  - Three consumer hooks (post-checkout, post-commit, post-merge) each
    delegate to it via a 2-line shim (shebang + . delegation).
  - Only the helper contains `git lfs` calls; consumers are pure shims.
  - When git-lfs is absent on PATH, each hook exits 2 with an error
    naming the calling hook (via `$(basename "$0")`).
  - When git-lfs is present, each hook runs `git lfs <verb>` with the
    forwarded args.

The tests intentionally do not assume any specific implementation beyond
the contract above; they exercise the helper through the consumer shims.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import stat
import subprocess

import pytest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[1]
HOOKS_DIR = REPO_ROOT / ".githooks"
HELPER = HOOKS_DIR / "_git-lfs-hook.sh"
CONSUMERS = ("post-checkout", "post-commit", "post-merge")


# ---------------------------------------------------------------------------
# Static-file structure
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("name", CONSUMERS)
def test_consumer_exists_and_is_executable(name: str) -> None:
    p = HOOKS_DIR / name
    assert p.exists(), f"missing consumer {name!r}"
    mode = p.stat().st_mode
    assert mode & stat.S_IXUSR, f"consumer {name!r} not user-executable"


def test_helper_exists_and_is_executable() -> None:
    assert HELPER.exists(), f"missing helper {HELPER!r}"
    mode = HELPER.stat().st_mode
    assert mode & stat.S_IXUSR, "helper is not user-executable"


@pytest.mark.parametrize("name", CONSUMERS)
def test_consumer_is_shebang_plus_delegation(name: str) -> None:
    """Each post-* hook is exactly 2 lines: shebang + `. delegation`."""
    p = HOOKS_DIR / name
    lines = [ln for ln in p.read_text().splitlines() if ln.strip()]
    assert len(lines) == 2, (
        f"expected exactly 2 non-blank lines in {name}, got {len(lines)}:\n"
        + "\n".join(lines)
    )
    assert lines[0].startswith("#!"), "first line must be a shebang"
    # The second line must source the canonical helper. We accept either
    # a bare `. ...` or a `VAR=val . ...` env-prefixed form (used to pass
    # the verb in a POSIX-sh-compatible way — dash does not forward args
    # to a `.`-sourced script, so the contract uses GIT_LFS_VERB).
    stripped = lines[1].lstrip()
    has_dot = (stripped.startswith(".") or stripped.startswith("GIT_LFS_VERB=")) and (
        " . " in stripped or stripped.startswith(". ")
    )
    assert has_dot, (
        f"second line of {name} must be a `. <delegate>` invocation "
        f"(optionally prefixed with GIT_LFS_VERB=...); got: {lines[1]!r}"
    )
    # The shim must reference the canonical helper, not inline `git lfs`.
    assert "_git-lfs-hook.sh" in lines[1], (
        f"second line of {name} must delegate to _git-lfs-hook.sh"
    )
    # Either the verb appears as a positional arg (`. helper.sh verb "$@"`)
    # OR is passed via the GIT_LFS_VERB env var (`GIT_LFS_VERB=verb . helper "$@"`).
    verb_via_arg = (
        f"_git-lfs-hook.sh {name}" in lines[1]
        or f"_git-lfs-hook.sh\" {name}" in lines[1]
    )
    verb_via_env = f"GIT_LFS_VERB={name}" in lines[1]
    assert verb_via_arg or verb_via_env, (
        f"second line of {name} must reference the verb ({name}) — either "
        f"as a positional arg or via GIT_LFS_VERB=...; got: {lines[1]!r}"
    )


def test_only_helper_contains_git_lfs_calls() -> None:
    """`rg 'git lfs'` should match the helper only — never the consumers."""
    import re

    pat = re.compile(r"\bgit\s+lfs\b")
    for name in ("_git-lfs-hook.sh",) + CONSUMERS:
        text = (HOOKS_DIR / name).read_text()
        hits = pat.findall(text)
        if name == "_git-lfs-hook.sh":
            assert hits, "helper must contain at least one `git lfs` call"
        else:
            assert not hits, (
                f"consumer {name!r} must NOT contain `git lfs` calls; "
                "it should delegate to the helper"
            )


# ---------------------------------------------------------------------------
# Behavior — fail-closed exit 2 with correct filename when git-lfs absent
# ---------------------------------------------------------------------------

@pytest.fixture
def empty_path() -> str:
    """Return a PATH that excludes every directory containing a `git-lfs`."""
    parts = []
    for d in os.environ.get("PATH", "").split(os.pathsep):
        if not d:
            continue
        candidate = pathlib.Path(d) / "git-lfs"
        if candidate.exists():
            continue
        parts.append(d)
    return os.pathsep.join(parts)


def _git_lfs_absent() -> bool:
    """Is `git-lfs` absent from PATH right now?"""
    return shutil.which("git-lfs") is None


@pytest.mark.skipif(
    shutil.which("git-lfs") is not None,
    reason="requires git-lfs to be absent on PATH",
)
def test_helper_exits_two_when_git_lfs_missing(capsys) -> None:
    proc = subprocess.run(
        [str(HELPER), "post-checkout"],
        capture_output=True,
        text=True,
        env={**os.environ, "PATH": "/nonexistent"},
    )
    assert proc.returncode == 2, (
        f"helper should exit 2 when git-lfs is missing, got {proc.returncode}"
    )
    combined = (proc.stdout or "") + (proc.stderr or "")
    assert "_git-lfs-hook.sh" in combined, (
        f"error should name the helper file, got: {combined!r}"
    )


@pytest.mark.parametrize("name", CONSUMERS)
@pytest.mark.skipif(
    shutil.which("git-lfs") is None,
    reason="requires git-lfs present on PATH to construct a negative case",
)
def test_consumer_exits_two_with_correct_filename_when_git_lfs_missing(
    name: str, empty_path: str
) -> None:
    """When git-lfs is absent, each hook must exit 2 AND name ITSELF in
    the error (not the helper's basename)."""
    hook = HOOKS_DIR / name
    proc = subprocess.run(
        [str(hook)],
        capture_output=True,
        text=True,
        env={**os.environ, "PATH": empty_path},
    )
    assert proc.returncode == 2, (
        f"{name} should exit 2 when git-lfs is missing; "
        f"got rc={proc.returncode}, stderr={proc.stderr!r}"
    )
    combined = (proc.stdout or "") + (proc.stderr or "")
    assert name in combined, (
        f"error should name the calling hook {name!r}; got: {combined!r}"
    )


@pytest.mark.parametrize("name", CONSUMERS)
@pytest.mark.skipif(
    shutil.which("git-lfs") is None,
    reason="requires git-lfs present on PATH to construct a negative case",
)
def test_consumer_exits_two_when_git_lfs_missing_no_such_path(
    name: str,
) -> None:
    """Same fail-closed invariant using an empty PATH — works regardless
    of whether git-lfs is installed."""
    hook = HOOKS_DIR / name
    proc = subprocess.run(
        [str(hook)],
        capture_output=True,
        text=True,
        env={k: v for k, v in os.environ.items() if k != "PATH"} | {"PATH": ""},
    )
    assert proc.returncode == 2, (
        f"{name} should exit 2 when PATH is empty; "
        f"got rc={proc.returncode}, stderr={proc.stderr!r}"
    )
    combined = (proc.stdout or "") + (proc.stderr or "")
    assert name in combined, (
        f"error should name the calling hook {name!r}; got: {combined!r}"
    )


# ---------------------------------------------------------------------------
# Behavior — when git-lfs is present, each hook runs `git lfs <verb>` with "$@"
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("name", CONSUMERS)
@pytest.mark.skipif(
    shutil.which("git-lfs") is None,
    reason="requires git-lfs present on PATH",
)
def test_consumer_invokes_git_lfs_with_verb_when_present(
    name: str, tmp_path: pathlib.Path
) -> None:
    """When git-lfs is present, each consumer must run `git lfs <verb>` and
    pass through any forwarded args. We shim `git` (and `git-lfs`) into fake
    binaries that record the invocation, and keep coreutil dirs in PATH so
    `dirname` resolves inside the consumer's delegation.
    """
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    record = tmp_path / "invocation.json"
    record.write_text("")

    git_lfs_script = "#!/bin/sh\nprintf '%s\\n' \"$@\" > " + str(record) + "\nexit 0\n"
    fake_git_lfs = fake_bin / "git-lfs"
    fake_git_lfs.write_text(git_lfs_script)
    fake_git_lfs.chmod(0o755)

    git_script = '#!/bin/sh\nif [ "$1" = lfs ]; then\n  shift\n  exec git-lfs "$@"\nfi\nexit 0\n'
    fake_git = fake_bin / "git"
    fake_git.write_text(git_script)
    fake_git.chmod(0o755)

    test_path = os.pathsep.join([str(fake_bin), "/usr/bin", "/bin"])

    hook = HOOKS_DIR / name
    proc = subprocess.run(
        [str(hook), "arg1", "arg2"],
        capture_output=True,
        text=True,
        env={**os.environ, "PATH": test_path},
    )
    assert proc.returncode == 0, (
        f"{name} should succeed when git-lfs shim is present; "
        f"rc={proc.returncode}, stderr={proc.stderr!r}"
    )
    argv = record.read_text().splitlines()
    assert argv and argv[0] == name, (
        f"git-lfs should be called with verb={name!r}; got argv={argv!r}"
    )
    assert "arg1" in argv and "arg2" in argv, (
        f"git-lfs should receive forwarded args; got argv={argv!r}"
    )
