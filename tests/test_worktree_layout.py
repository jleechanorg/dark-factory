"""TDD tests for runner.worktree_layout — agent worktree isolation + reaper.

Bead jleechan-jw4c acceptance criteria:

  1. Agent worktrees MUST live OUTSIDE the primary checkout (e.g.
     ~/.worktrees/<repo>/<agent-id>/, NOT <primary>/.claude/worktrees/).
  2. A reaper must cap the number of agent worktrees (no unbounded
     growth like the 511-entry Jeff-Ubuntu measurement).
  3. A reaper must reap entries older than a TTL (no TTL was a root
     cause of unbounded accumulation).
  4. Tools must detect cross-worktree writes — when a process with a
     designated agent worktree touches a path under the primary
     checkout that isn't shared via stash/rebase, the helper flags it.

These tests are RED until runner/worktree_layout.py lands.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys
import time
from pathlib import Path

import pytest


# Make `runner` importable when pytest runs this file directly from a
# worktree that doesn't have a top-level venv yet.
ROOT = pathlib.Path(__file__).resolve().parent.parent
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from runner.worktree_layout import (  # noqa: E402  (intentional: sys.path tweak above)
    AgentWorktreeLayout,
    CrossWorktreeWrite,
    ReapedWorktree,
    apply_reap,
    is_worktree_protected,
    protected_worktree_paths,
    reap_stale_worktrees,
)


# ---------------------------------------------------------------------------
# 1. Isolation — agent worktree root must NOT be inside the primary checkout.
# ---------------------------------------------------------------------------


def test_agent_worktree_root_is_outside_primary_checkout(tmp_path: Path) -> None:
    """The default agent worktree root for a fresh repo must point OUTSIDE
    the primary working directory — never under `.claude/worktrees/` inside it.

    Reproduces the 511-worktree Jeff-Ubuntu measurement, where 453 of 511
    registrations lived at `~/projects/dark-factory/.claude/worktrees/agent-*`
    inside the primary checkout.
    """
    primary = tmp_path / "primary"
    primary.mkdir()
    (primary / "marker").write_text("inside-primary")

    layout = AgentWorktreeLayout.for_repo(primary, agent_id="agent-test")

    root = layout.root
    assert primary.resolve() not in root.resolve().parents, (
        f"agent worktree root {root} MUST NOT live under primary checkout {primary}"
    )
    # Specifically: must NOT pick the legacy /<repo>/.claude/worktrees/ path.
    assert ".claude/worktrees" not in str(root), (
        f"agent worktree root {root} still uses the legacy .claude/worktrees layout"
    )


def test_agent_worktree_root_respects_override(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Operator can override the root via $DARK_FACTORY_AGENT_WORKTREES.

    Use case: an isolated CI runner pins the root to /var/lib/df-worktrees
    to keep agent checkouts off the build agent's home filesystem.
    """
    primary = tmp_path / "primary"
    primary.mkdir()
    override = tmp_path / "scratch"
    override.mkdir()
    monkeypatch.setenv("DARK_FACTORY_AGENT_WORKTREES", str(override))

    layout = AgentWorktreeLayout.for_repo(primary, agent_id="agent-test")

    assert layout.root.parent.resolve() == override.resolve()
    assert "agent-test" in layout.root.name


def test_agent_worktree_path_rejects_within_primary(tmp_path: Path) -> None:
    """`AgentWorktreeLayout.path_for(...)` for a path inside the primary
    checkout raises — call sites must use a path explicitly authorized for
    cross-worktree sharing (e.g. via `authorize_cross_worktree_path`).
    """
    primary = tmp_path / "primary"
    primary.mkdir()
    (primary / "shared.txt").write_text("ok")
    layout = AgentWorktreeLayout.for_repo(primary, agent_id="agent-test")

    with pytest.raises(ValueError, match="outside the primary checkout"):
        layout.path_for(primary / "shared.txt")


# ---------------------------------------------------------------------------
# 2. & 3. Reaper — TTL + cap.
# ---------------------------------------------------------------------------


def _make_fake_worktree(
    parent: Path, name: str, *, age_seconds: int, modified: bool = True
) -> Path:
    """Materialize a directory with controllable mtime + a `.git` file
    so reap_stale_worktrees treats it as a worktree registration."""
    wt = parent / name
    wt.mkdir()
    (wt / ".git").write_text("gitdir: /tmp/fake\n")
    (wt / "README").write_text(name)
    if modified:
        ts = time.time() - age_seconds
        os.utime(wt, (ts, ts))
    return wt


def test_reaper_removes_entries_older_than_ttl(tmp_path: Path) -> None:
    primary = tmp_path / "primary"
    primary.mkdir()
    root = tmp_path / "agent-worktrees"
    root.mkdir()

    fresh = _make_fake_worktree(root, "agent-fresh", age_seconds=60)
    stale = _make_fake_worktree(
        root, "agent-stale", age_seconds=60 * 60 * 24 * 30
    )  # 30 days

    summary = reap_stale_worktrees(
        root=root,
        primary_checkout=primary,
        ttl_seconds=60 * 60 * 24 * 7,  # 7 days
        max_count=None,
    )
    apply_reap(summary)  # actually delete

    assert fresh.exists(), "fresh entry must survive"
    assert not stale.exists(), "stale entry beyond TTL must be removed"
    assert any(
        isinstance(r, ReapedWorktree) and r.path == stale for r in summary.removed
    ), "summary must record the removed entry"


def test_reaper_caps_total_count(tmp_path: Path) -> None:
    """When max_count is set, the reaper must enforce the cap by deleting
    oldest entries first.
    """
    primary = tmp_path / "primary"
    primary.mkdir()
    root = tmp_path / "agent-worktrees"
    root.mkdir()

    # Make 5 entries with controlled mtimes; cap will be 2.
    entries = [
        _make_fake_worktree(root, f"agent-{i:02d}", age_seconds=10 * (i + 1))
        for i in range(5)
    ]

    summary = reap_stale_worktrees(
        root=root,
        primary_checkout=primary,
        ttl_seconds=None,
        max_count=2,
    )
    apply_reap(summary)  # actually delete

    survivors = [e for e in entries if e.exists()]
    assert len(survivors) == 2, (
        f"expected 2 survivors; got {[e.name for e in survivors]}"
    )
    # Two newest (lowest age) survive: agent-00 (10s) and agent-01 (20s)
    survivor_names = sorted(e.name for e in survivors)
    assert survivor_names == ["agent-00", "agent-01"], survivor_names
    assert len(summary.removed) == 3


def test_reaper_skips_paths_inside_primary_checkout(tmp_path: Path) -> None:
    """Defense in depth: if a `.claude/worktrees/agent-*` directory exists
    inside the primary checkout (the legacy bug), the reaper refuses to
    delete it. The fix moves new worktrees OUTSIDE; the reaper is the
    forward-only reaper, not a retroactive scrubber.
    """
    primary = tmp_path / "primary"
    primary.mkdir()
    nested = primary / ".claude" / "worktrees"
    nested.mkdir(parents=True)
    legit_but_stale = _make_fake_worktree(nested, "agent-stale", age_seconds=10**9)

    summary = reap_stale_worktrees(
        root=nested,  # the legacy root
        primary_checkout=primary,
        ttl_seconds=0,
        max_count=None,
    )

    assert legit_but_stale.exists(), "reaper must not auto-delete nested legacy dirs"
    assert summary.refused, "summary must record the refusal"
    assert summary.removed == []


# ---------------------------------------------------------------------------
# 4. Cross-worktree write detection.
# ---------------------------------------------------------------------------


def test_is_within_target_worktree_flags_outside_paths(tmp_path: Path) -> None:
    """When the agent worktree is at <root>/agent-test/, paths under
    <root>/agent-other/ are 'outside' the target worktree.
    """
    root = tmp_path / "agent-worktrees"
    mine = root / "agent-test"
    theirs = root / "agent-other"
    mine.mkdir(parents=True)
    theirs.mkdir(parents=True)

    layout = AgentWorktreeLayout(root=mine, primary_checkout=tmp_path / "primary")

    assert layout.is_within_target_worktree(mine / "src" / "foo.py") is True
    assert layout.is_within_target_worktree(theirs / "src" / "foo.py") is False
    assert layout.is_within_target_worktree(Path("/etc/passwd")) is False


def test_detect_cross_worktree_write_raises(tmp_path: Path) -> None:
    """A write observed in a SIBLING agent worktree is flagged.
    Reproduces the df-372 incident: that session had its own worktree
    (~/.worktrees/dark-factory/df-372) yet writes landed in the shared
    primary checkout.
    """
    primary = tmp_path / "primary"
    primary.mkdir()
    root = tmp_path / "agent-worktrees"
    mine = root / "agent-mine"
    other = root / "agent-other"
    mine.mkdir(parents=True)
    other.mkdir(parents=True)

    layout = AgentWorktreeLayout(root=mine, primary_checkout=primary)

    with pytest.raises(CrossWorktreeWrite):
        layout.detect_cross_worktree_write(other / "daemon" / "src" / "tick.rs")


def test_detect_cross_worktree_write_allows_authorized_shared_paths(
    tmp_path: Path,
) -> None:
    """An explicitly authorized shared path (e.g. an artifacts/ dir under
    the primary checkout, used for evidence exchange) MUST NOT raise.
    """
    primary = tmp_path / "primary"
    primary.mkdir()
    shared = primary / "evidence"
    shared.mkdir()
    root = tmp_path / "agent-worktrees"
    mine = root / "agent-mine"
    mine.mkdir(parents=True)

    layout = AgentWorktreeLayout(root=mine, primary_checkout=primary)
    layout.authorize_cross_worktree_path(shared)

    # No exception.
    layout.detect_cross_worktree_write(shared / "bundle.json")


# ---------------------------------------------------------------------------
# 5. CLI smoke — `bin/df-reap-worktrees` exists and is executable.
# ---------------------------------------------------------------------------


def test_cli_binary_exists_and_is_executable() -> None:
    cli = ROOT / "bin" / "df-reap-worktrees"
    assert cli.exists(), f"{cli} must exist (bead jleechan-jw4c acceptance)"
    assert os.access(cli, os.X_OK), f"{cli} must be executable"


def test_cli_reap_dry_run_does_not_delete(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    primary = tmp_path / "primary"
    primary.mkdir()
    root = tmp_path / "agent-worktrees"
    root.mkdir()
    stale = _make_fake_worktree(root, "agent-stale", age_seconds=10**8)

    cli = ROOT / "bin" / "df-reap-worktrees"
    env = {**os.environ, "DARK_FACTORY_AGENT_WORKTREES": str(root)}
    result = subprocess.run(
        [
            sys.executable,
            str(cli),
            "--primary-checkout",
            str(primary),
            "--ttl-seconds",
            "1",
            "--max-count",
            "0",
            "--dry-run",
        ],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )

    assert result.returncode == 0, f"{result.stdout}\n{result.stderr}"
    assert stale.exists(), "dry-run MUST NOT delete"
    out = (result.stdout + result.stderr).lower()
    assert "would remove" in out or "dry-run" in out or "would reap" in out


# ---------------------------------------------------------------------------
# 6. Protect-list guard — jleechan-jw4c: the reaper must never prune a
#    worktree that a live (ENABLED) systemd --user unit depends on. The
#    concrete incident this reproduces: dark-factory-merge-guard.service's
#    WorkingDirectory IS an agent worktree
#    (~/projects/dark-factory-worktrees/fix-jleechan-s3c-merge-guard-scheduler);
#    that worktree is the ONLY code path that runs `gh pr merge`. Pruning it
#    silently kills merge authority while the timer keeps firing no-ops.
# ---------------------------------------------------------------------------


def _make_systemd_unit_dir(
    tmp_path: Path, *, service_name: str, working_directory: Path, enabled: bool
) -> Path:
    """Build a minimal fake ``~/.config/systemd/user``-shaped tree with one
    oneshot .service + .timer pair, mirroring the REAL merge-guard shape
    (the .service has no [Install] section of its own — only the .timer is
    enabled, via a symlink in timers.target.wants/, and Unit= chains back
    to the service). "Enabled" is toggled by whether that symlink exists.
    """
    unit_dir = tmp_path / "systemd-user"
    unit_dir.mkdir(parents=True, exist_ok=True)
    timer_name = service_name.replace(".service", ".timer")

    (unit_dir / service_name).write_text(
        "[Unit]\n"
        f"Description=fake {service_name}\n"
        "[Service]\n"
        "Type=oneshot\n"
        f"WorkingDirectory={working_directory}\n"
        f"ExecStart=/bin/bash {working_directory}/run.sh\n"
    )
    (unit_dir / timer_name).write_text(
        "[Unit]\n"
        f"Unit={service_name}\n"
        "[Timer]\n"
        "OnUnitActiveSec=60s\n"
        "[Install]\n"
        "WantedBy=timers.target\n"
    )

    if enabled:
        wants = unit_dir / "timers.target.wants"
        wants.mkdir(exist_ok=True)
        (wants / timer_name).symlink_to(unit_dir / timer_name)

    return unit_dir


def test_protected_worktree_paths_finds_enabled_timer_triggered_service(
    tmp_path: Path,
) -> None:
    """Reproduces the real merge-guard shape: only the .timer is enabled
    (symlinked under timers.target.wants/), the .service has no [Install]
    section, yet its WorkingDirectory must still come back as protected.
    """
    worktree = tmp_path / "fix-jleechan-s3c-merge-guard-scheduler"
    worktree.mkdir()
    unit_dir = _make_systemd_unit_dir(
        tmp_path,
        service_name="dark-factory-merge-guard.service",
        working_directory=worktree,
        enabled=True,
    )

    protected = protected_worktree_paths(systemd_user_dir=unit_dir)

    assert worktree.resolve() in protected, (
        f"WorkingDirectory of an enabled-timer-triggered service must be protected; "
        f"got {protected}"
    )


def test_protected_worktree_paths_ignores_disabled_units(tmp_path: Path) -> None:
    """A unit that exists on disk but is NOT enabled (no *.wants/ symlink)
    must not protect its paths — otherwise every worktree ever referenced
    by a unit file (even a disabled/stale one) becomes unprunable forever.
    """
    worktree = tmp_path / "disabled-unit-worktree"
    worktree.mkdir()
    unit_dir = _make_systemd_unit_dir(
        tmp_path,
        service_name="disabled-guard.service",
        working_directory=worktree,
        enabled=False,
    )

    protected = protected_worktree_paths(systemd_user_dir=unit_dir)

    assert worktree.resolve() not in protected


def test_is_worktree_protected_boundary_safe_prefix_match(tmp_path: Path) -> None:
    """The exact near-miss the reaper must not fall for: a protected path
    of `.../a/b` must NOT match a candidate worktree `.../a/bc`, even
    though 'bc' starts with the string 'b'. Plain str.startswith() on the
    unresolved paths would wrongly match; the boundary-safe check must not.
    """
    base = tmp_path / "a"
    base.mkdir()
    protected_path = base / "b"
    protected_path.mkdir()
    near_miss = base / "bc"
    near_miss.mkdir()
    nested_under_protected = protected_path / "sub"
    nested_under_protected.mkdir()

    assert is_worktree_protected(protected_path, [protected_path]) is True
    assert is_worktree_protected(nested_under_protected, [protected_path]) is True
    assert is_worktree_protected(near_miss, [protected_path]) is False


def test_reaper_refuses_worktree_referenced_by_enabled_systemd_unit(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    """End-to-end: a worktree that is stale by TTL but referenced by an
    enabled systemd unit must land in `refused`, survive on disk, and the
    refusal must be LOGGED (never fail silently).
    """
    primary = tmp_path / "primary"
    primary.mkdir()
    root = tmp_path / "agent-worktrees"
    root.mkdir()

    guarded = _make_fake_worktree(root, "merge-guard-worktree", age_seconds=10**8)
    ordinary_stale = _make_fake_worktree(root, "ordinary-stale", age_seconds=10**8)

    unit_dir = _make_systemd_unit_dir(
        tmp_path,
        service_name="dark-factory-merge-guard.service",
        working_directory=guarded,
        enabled=True,
    )
    protected = protected_worktree_paths(systemd_user_dir=unit_dir)
    assert guarded.resolve() in protected  # sanity: fixture wired correctly

    with caplog.at_level("WARNING"):
        summary = reap_stale_worktrees(
            root=root,
            primary_checkout=primary,
            ttl_seconds=60,
            max_count=None,
            protected_paths=protected,
        )
    apply_reap(summary)

    assert guarded.exists(), "systemd-protected worktree must NOT be pruned"
    assert not ordinary_stale.exists(), "unprotected stale worktree must still be reaped"
    assert any(
        r.path == guarded and "systemd" in r.reason for r in summary.refused
    ), summary.refused
    assert any(
        str(guarded) in rec.message and "refus" in rec.message.lower()
        for rec in caplog.records
    ), "refusal must be logged, not silent"


# ---------------------------------------------------------------------------
# 7. Git safety guard — locked / dirty / unpushed worktrees must never be
#    pruned even when TTL/count would otherwise select them. Uses REAL git
#    worktrees (not the `.git`-file fake used above) so lock/dirty/unpushed
#    state is exercised through actual git, not a mock.
# ---------------------------------------------------------------------------


def _git(cwd: Path, *args: str) -> subprocess.CompletedProcess:
    result = subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, (
        f"git {' '.join(args)} in {cwd} failed:\n{result.stdout}\n{result.stderr}"
    )
    return result


def _make_real_git_worktree(
    tmp_path: Path,
    wt_root: Path,
    name: str,
    *,
    ahead: int = 0,
    dirty: bool = False,
    locked: bool = False,
    push: bool = True,
) -> Path:
    """Build a real origin + main clone + a linked worktree under
    ``wt_root``, with controllable locked/dirty/unpushed state via actual
    git operations (not mocked)."""
    origin = tmp_path / f"{name}-origin.git"
    if not origin.exists():
        _git(tmp_path, "init", "--bare", str(origin))

    main_repo = tmp_path / f"{name}-main"
    _git(tmp_path, "clone", "-q", str(origin), str(main_repo))
    _git(main_repo, "config", "user.email", "test@example.com")
    _git(main_repo, "config", "user.name", "Test")
    (main_repo / "README.md").write_text("init\n")
    _git(main_repo, "add", "README.md")
    _git(main_repo, "commit", "-q", "-m", "init")
    # Use whatever the local default branch actually is (git's
    # init.defaultBranch varies across machines/versions) rather than
    # assuming "main" — pushing under a hardcoded name we then reference
    # as a *local* start-point can trip `git worktree add`'s remote-branch
    # DWIM resolution and silently create the wrong branch.
    base_branch = _git(main_repo, "symbolic-ref", "--short", "HEAD").stdout.strip()
    _git(main_repo, "push", "-q", "-u", "origin", base_branch)

    wt_root.mkdir(parents=True, exist_ok=True)
    wt_path = wt_root / name
    branch = f"feature-{name}"
    _git(main_repo, "worktree", "add", "-q", "-b", branch, str(wt_path), base_branch)
    _git(wt_path, "config", "user.email", "test@example.com")
    _git(wt_path, "config", "user.name", "Test")

    if push:
        _git(wt_path, "push", "-q", "-u", "origin", branch)

    if ahead:
        for i in range(ahead):
            (wt_path / f"file{i}.txt").write_text("x")
            _git(wt_path, "add", ".")
            _git(wt_path, "commit", "-q", "-m", f"local commit {i}")

    if dirty:
        (wt_path / "dirty.txt").write_text("uncommitted\n")

    if locked:
        _git(main_repo, "worktree", "lock", str(wt_path))

    # Backdate mtime so it's TTL-stale regardless of git-op ordering.
    ts = time.time() - 10**8
    os.utime(wt_path, (ts, ts))

    return wt_path


def test_reaper_refuses_locked_dirty_and_unpushed_worktrees(tmp_path: Path) -> None:
    primary = tmp_path / "primary"
    primary.mkdir()
    wt_root = tmp_path / "agent-worktrees"

    locked = _make_real_git_worktree(tmp_path, wt_root, "locked-wt", locked=True)
    dirty = _make_real_git_worktree(tmp_path, wt_root, "dirty-wt", dirty=True)
    unpushed = _make_real_git_worktree(tmp_path, wt_root, "unpushed-wt", ahead=1)
    clean = _make_real_git_worktree(tmp_path, wt_root, "clean-wt")

    summary = reap_stale_worktrees(
        root=wt_root,
        primary_checkout=primary,
        ttl_seconds=60,
        max_count=None,
        check_git_safety=True,
    )
    apply_reap(summary)

    assert locked.exists(), "locked worktree must NOT be pruned"
    assert dirty.exists(), "dirty (uncommitted changes) worktree must NOT be pruned"
    assert unpushed.exists(), "worktree with unpushed commits must NOT be pruned"
    assert not clean.exists(), "an ordinary stale CLEAN worktree must still be prunable"

    refused_paths = {r.path for r in summary.refused}
    assert locked in refused_paths
    assert dirty in refused_paths
    assert unpushed in refused_paths
    assert clean not in refused_paths
