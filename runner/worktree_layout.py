"""Canonical agent worktree layout + reaper (bead jleechan-jw4c).

Follow-up that wires the CLI to the safety guards as the DEFAULT:
bead jleechan-6i73 / PR #601. Without it, the CLI reaped exactly as
before even after the guards landed — including the worktree that holds
the only `gh pr merge` call site. Now `bin/df-reap-worktrees` defaults
to `protected_paths=protected_worktree_paths()` AND
`check_git_safety=True`; opt-outs are `--no-systemd-protect` and
`--no-git-safety`.

Evidence: https://gist.github.com/jleechan2015/8f268bc2359f09b5d7c2a3c049f5b632

Three forces this module addresses:

1.  Agent worktrees MUST live OUTSIDE the primary checkout. The previous
    convention was `<primary>/.claude/worktrees/agent-*`, which buried
    nested clones inside the repo (24G measured on Jeff-Ubuntu 2026-08-05).
    New convention: ``$DARK_FACTORY_AGENT_WORKTREES`` (default
    ``~/.worktrees/<repo>/<agent-id>/``). Override-able per environment.

2.  A reaper must bound both the COUNT and AGE of agent worktrees. The
    spawn-queue outage (jleechan-la67, 2026-07-08) showed an unbounded
    producer with no consumer is an outage timer — measured in disk
    instead of latency this time, but the same shape. The reaper is
    forward-only: it NEVER deletes a directory that lives under the
    primary checkout (legacy nested dirs must be moved by hand).

3.  A cross-worktree write must be detectable. The df-372 incident
    (2026-08-05) showed a session with its own worktree writing into
    the shared primary checkout. ``AgentWorktreeLayout.detect_cross_worktree_write``
    raises when a path is touched that is neither inside the target
    worktree nor on an allowlist of explicitly shared paths (e.g. an
    artifacts/ directory the operator designated for evidence exchange).

The module deliberately avoids pulling from any other runner submodule
— keeps it pure-stdlib so it can be exercised under minimal test env.
"""

from __future__ import annotations

import argparse
import errno
import logging
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable, Optional, Sequence


ENV_AGENT_WORKTREES_ROOT = "DARK_FACTORY_AGENT_WORKTREES"
ENV_AGENT_WORKTREES_TTL = "DARK_FACTORY_AGENT_WORKTREES_TTL_DAYS"
ENV_AGENT_WORKTREES_MAX = "DARK_FACTORY_AGENT_WORKTREES_MAX_COUNT"

logger = logging.getLogger("dark_factory.worktree_layout")


# Legacy location that the convention SHIFTED AWAY from. Present so the
# reaper can refuse to act on it (forward-only).
LEGACY_NESTED_DIR = ".claude/worktrees"


class CrossWorktreeWrite(Exception):
    """Raised when a write path is neither inside the target worktree nor
    on an explicit allowlist. Detected via the path-string; this helper
    is not a syscall firewall — call sites combine it with their own
    pre-commit / dispatch guards.
    """


@dataclass(frozen=True)
class ReapedWorktree:
    path: Path
    reason: str  # "ttl" | "count" | "refused"


@dataclass
class ReapSummary:
    root: Path
    removed: list[ReapedWorktree] = field(default_factory=list)
    refused: list[ReapedWorktree] = field(default_factory=list)
    survivors: list[Path] = field(default_factory=list)

    def human_lines(self) -> list[str]:
        lines = [f"ReapSummary for {self.root}"]
        lines.append(f"  removed: {len(self.removed)}")
        for r in self.removed:
            lines.append(f"    - {r.path}  ({r.reason})")
        lines.append(f"  refused: {len(self.refused)}")
        for r in self.refused:
            lines.append(f"    - {r.path}  ({r.reason})")
        lines.append(f"  survivors: {len(self.survivors)}")
        return lines


@dataclass
class AgentWorktreeLayout:
    """Resolved layout for a single agent's worktree.

    Construct via ``AgentWorktreeLayout.for_repo(primary, agent_id=...)``
    in production paths; the explicit constructor is for tests.
    """

    root: Path
    primary_checkout: Path
    _authorized_shared: tuple[Path, ...] = ()

    # ------------------------------------------------------------------ factory

    @classmethod
    def for_repo(
        cls,
        primary_checkout: Path,
        *,
        agent_id: str,
        override_root: Optional[Path] = None,
    ) -> "AgentWorktreeLayout":
        """Return the layout for a fresh agent.

        Resolution order for the root:

          1. ``override_root`` (programmatic — used by tests/CLI).
          2. ``$DARK_FACTORY_AGENT_WORKTREES`` environment variable.
          3. ``~/.worktrees/<repo-slug>/<agent_id>``.

        The resulting root is ALWAYS outside the primary checkout — the
        factory deliberately REFUSES to return a path whose parent chain
        contains ``<primary>/.claude/worktrees/``.
        """
        primary = Path(primary_checkout).expanduser().resolve()
        repo_slug = _repo_slug(primary)
        chosen = _resolve_agent_root(repo_slug, agent_id, override=override_root)

        # Forward-only invariant: refuse the legacy nested path.
        primary_with_sep = str(primary) + os.sep
        chosen_str = str(chosen)
        if (
            chosen_str.startswith(primary_with_sep)
            and LEGACY_NESTED_DIR in Path(chosen_str).parts
        ):
            raise ValueError(
                f"agent worktree root {chosen} lives under the legacy nested "
                f"location {primary}/{LEGACY_NESTED_DIR}/; this factory refuses to "
                f"create new agent worktrees inside the primary checkout."
            )

        return cls(root=chosen, primary_checkout=primary)

    # ----------------------------------------------------------- introspection

    def is_within_target_worktree(self, path: Path) -> bool:
        """True if ``path`` resolves inside ``self.root``.

        Uses Path.is_relative_to when available; falls back to lexical
        prefix matching against the resolved-root string with a trailing
        separator to avoid `/tmp/worktree2` matching `/tmp/worktree`.
        """
        try:
            return Path(path).expanduser().resolve().is_relative_to(self.root)
        except AttributeError:  # py<3.9 fallback (defensive)
            resolved = str(Path(path).expanduser().resolve())
            root_str = str(self.root) + os.sep
            return resolved.startswith(root_str) or resolved == str(self.root)

    def authorize_cross_worktree_path(self, path: Path) -> None:
        """Allowlist a path that may legitimately be written from
        outside the target worktree. Common case: ``<primary>/evidence/``."""
        new_entry = path.expanduser().resolve()
        object.__setattr__(
            self, "_authorized_shared", self._authorized_shared + (new_entry,)
        )

    def detect_cross_worktree_write(self, path: Path) -> None:
        """Raise :class:`CrossWorktreeWrite` unless ``path`` is inside
        the target worktree OR on the explicit allowlist."""
        target = Path(path).expanduser().resolve()
        if self.is_within_target_worktree(target):
            return
        for allowed in self._authorized_shared:
            try:
                if target.is_relative_to(allowed):
                    return
            except AttributeError:
                if str(target).startswith(str(allowed) + os.sep):
                    return
        raise CrossWorktreeWrite(
            f"path {target} is outside target worktree {self.root} and not on the "
            f"allowlist ({[str(p) for p in self._authorized_shared]})"
        )

    # ----------------------------------------------------------- path helpers

    def path_for(self, path: Path) -> Path:
        """Resolve a path inside the target worktree. Raises if the path
        falls outside the worktree."""
        candidate = Path(path).expanduser().resolve()
        if self.is_within_target_worktree(candidate):
            return candidate
        raise ValueError(
            f"path {candidate} is outside the primary checkout's worktree {self.root}; "
            f"call sites that need to touch the primary checkout must use "
            f"detect_cross_worktree_write + authorize_cross_worktree_path explicitly."
        )


# ---------------------------------------------------------------------------
# Protect-list guard (jleechan-jw4c) — a systemd --user unit's WorkingDirectory
# (or any path it references) is a LIVE dependency, not a disposable checkout.
# Concrete incident this guards against: dark-factory-merge-guard.service's
# WorkingDirectory IS an agent worktree
# (~/projects/dark-factory-worktrees/fix-jleechan-s3c-merge-guard-scheduler)
# — the ONLY code path that runs `gh pr merge`. If the reaper prunes that
# directory, merge authority vanishes SILENTLY: the timer keeps firing every
# 60s, `cd` into a directory that no longer exists, and no-ops forever.
# ---------------------------------------------------------------------------


# Only extract path tokens from directives that are actually path-bearing
# (as opposed to e.g. Documentation=https://... or Description=...) — this
# is precise rather than "any absolute path anywhere in the file" so it
# doesn't false-positive on URLs while still generalizing beyond a literal
# /home/ prefix (unit files are commonly exercised against tmp dirs in
# tests, which don't live under /home).
_PATH_BEARING_UNIT_KEYS = frozenset(
    {
        "WorkingDirectory",
        "ExecStart",
        "ExecStartPre",
        "ExecStartPost",
        "ExecStop",
        "ExecStopPost",
        "ExecReload",
        "ReadWritePaths",
        "ReadOnlyPaths",
        "BindPaths",
        "BindReadOnlyPaths",
        "RootDirectory",
    }
)
_ABS_PATH_TOKEN_RE = re.compile(r"/[^\s\"']+")


def _enabled_unit_names(systemd_user_dir: Path) -> set[str]:
    """Unit names that are ENABLED, derived structurally from symlinks
    under ``*.wants/`` directories — this is exactly what `systemctl
    --user enable` creates, so it mirrors `systemctl --user is-enabled`
    without shelling out (keeps this unit-testable with a fake unit-dir
    tree and avoids a hard runtime dependency on systemd being present).
    """
    names: set[str] = set()
    if not systemd_user_dir.exists():
        return names
    for wants_dir in systemd_user_dir.glob("*.wants"):
        if not wants_dir.is_dir():
            continue
        for entry in wants_dir.iterdir():
            names.add(entry.name)
    return names


def _timer_target_service(timer_file: Path) -> str:
    """The .service unit a .timer triggers: explicit ``Unit=`` line, else
    the timer's own basename with .service instead of .timer (systemd's
    own default resolution). This covers the merge-guard's actual shape:
    the .service has no [Install] section of its own — only the .timer
    that points at it is ever "enabled".
    """
    try:
        text = timer_file.read_text()
    except OSError:
        return timer_file.stem + ".service"
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("Unit="):
            return line.split("=", 1)[1].strip()
    return timer_file.stem + ".service"


def _paths_referenced_by_unit(unit_file: Path) -> set[Path]:
    try:
        text = unit_file.read_text()
    except OSError:
        return set()
    found: set[Path] = set()
    for line in text.splitlines():
        line = line.strip()
        if "=" not in line:
            continue
        key, _, value = line.partition("=")
        if key not in _PATH_BEARING_UNIT_KEYS:
            continue
        for raw in _ABS_PATH_TOKEN_RE.findall(value):
            cleaned = raw.rstrip(",;")
            try:
                found.add(Path(cleaned).expanduser().resolve())
            except (OSError, RuntimeError):
                continue
    return found


def protected_worktree_paths(systemd_user_dir: Optional[Path] = None) -> set[Path]:
    """Filesystem paths referenced by ENABLED systemd --user units.

    Scans every unit under ``systemd_user_dir`` (default
    ``~/.config/systemd/user``) that is either itself enabled or is a
    .service triggered by an enabled .timer, and extracts every absolute
    path token (``WorkingDirectory=``, ``ExecStart=``, etc.) referenced in
    its unit file. The reaper refuses to prune anything in the returned
    set — see the module-level rationale above.
    """
    resolved_dir = (
        Path(systemd_user_dir)
        if systemd_user_dir is not None
        else Path.home() / ".config" / "systemd" / "user"
    ).expanduser()
    if not resolved_dir.exists():
        return set()

    enabled = _enabled_unit_names(resolved_dir)
    live_units: set[str] = set(enabled)
    for name in enabled:
        if name.endswith(".timer"):
            timer_file = resolved_dir / name
            if timer_file.exists():
                live_units.add(_timer_target_service(timer_file))

    paths: set[Path] = set()
    for name in sorted(live_units):
        unit_file = resolved_dir / name
        if unit_file.exists():
            paths |= _paths_referenced_by_unit(unit_file)
    return paths


def is_worktree_protected(path: Path, protected_paths: Iterable[Path]) -> bool:
    """True if ``path`` equals, contains, or is contained by any entry in
    ``protected_paths``, using BOUNDARY-safe comparison.

    Boundary-safety matters: a protected path of ``.../a/b`` must NOT
    match a candidate ``.../a/bc`` — plain string-prefix comparison on the
    unresolved paths would wrongly match on that near-miss.
    """
    resolved = Path(path).expanduser().resolve()
    for protected in protected_paths:
        p = Path(protected).expanduser().resolve()
        if resolved == p:
            return True
        try:
            if p.is_relative_to(resolved) or resolved.is_relative_to(p):
                return True
        except AttributeError:  # py<3.9 fallback (defensive)
            resolved_sep = str(resolved) + os.sep
            p_sep = str(p) + os.sep
            if str(p).startswith(resolved_sep) or str(resolved).startswith(p_sep):
                return True
    return False


# ---------------------------------------------------------------------------
# Git safety guard — locked / dirty (uncommitted) / unpushed worktrees must
# never be pruned even when TTL/count would otherwise select them. Fails
# CLOSED: any git command this can't positively confirm is treated as the
# unsafe condition it was checking for.
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class WorktreeGitStatus:
    locked: bool
    dirty: bool
    unpushed: bool


def _run_git(args: Sequence[str], cwd: Path) -> "subprocess.CompletedProcess[str]":
    return subprocess.run(
        ["git", "-C", str(cwd), *args],
        capture_output=True,
        text=True,
        check=False,
    )


def git_worktree_status(path: Path) -> WorktreeGitStatus:
    """Inspect a real git worktree's safety-relevant status: locked (per
    ``git worktree list --porcelain``), dirty (uncommitted changes), and
    unpushed (commits — or the entire branch — not present on the
    upstream). Any git command that errors (bogus/missing .git, not a
    worktree, git unavailable) is treated as the unsafe outcome, so a
    worktree the reaper cannot positively verify is never prunable.
    """
    path = Path(path)

    locked = True
    list_proc = _run_git(["worktree", "list", "--porcelain"], path)
    if list_proc.returncode == 0:
        try:
            resolved = str(path.resolve())
        except OSError:
            resolved = str(path)
        for block in list_proc.stdout.split("\n\n"):
            lines = block.splitlines()
            if not lines or not lines[0].startswith("worktree "):
                continue
            wt_path = lines[0][len("worktree "):].strip()
            try:
                if str(Path(wt_path).resolve()) != resolved:
                    continue
            except OSError:
                continue
            locked = any(line == "locked" or line.startswith("locked ") for line in lines[1:])
            break

    dirty = True
    status_proc = _run_git(["status", "--porcelain"], path)
    if status_proc.returncode == 0:
        dirty = bool(status_proc.stdout.strip())

    unpushed = True
    upstream_proc = _run_git(
        ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], path
    )
    if upstream_proc.returncode == 0:
        count_proc = _run_git(["rev-list", "--count", "@{u}..HEAD"], path)
        if count_proc.returncode == 0:
            unpushed = count_proc.stdout.strip() != "0"

    return WorktreeGitStatus(locked=locked, dirty=dirty, unpushed=unpushed)


def is_worktree_safe_to_prune(
    path: Path,
    *,
    protected_paths: Iterable[Path] = (),
    status: Optional[WorktreeGitStatus] = None,
) -> tuple[bool, str]:
    """Combine the systemd protect-list and git safety checks. Returns
    ``(safe, reason)`` — ``reason`` explains the refusal when unsafe.
    """
    if is_worktree_protected(path, protected_paths):
        return False, "referenced by an enabled systemd unit"
    st = status if status is not None else git_worktree_status(path)
    if st.locked:
        return False, "worktree is locked (git worktree list --porcelain)"
    if st.dirty:
        return False, "worktree has uncommitted changes"
    if st.unpushed:
        return False, "worktree has unpushed commits (or no upstream to verify against)"
    return True, "safe to prune"


# ---------------------------------------------------------------------------
# Reaper
# ---------------------------------------------------------------------------


def reap_stale_worktrees(
    *,
    root: Path,
    primary_checkout: Path,
    ttl_seconds: Optional[int],
    max_count: Optional[int],
    now: Optional[float] = None,
    protected_paths: Iterable[Path] = (),
    check_git_safety: bool = False,
    git_status_provider: Callable[[Path], WorktreeGitStatus] = git_worktree_status,
) -> ReapSummary:
    """Prune stale entries under ``root``.

    Forward-only: directories that resolve INSIDE ``primary_checkout``
    are recorded as ``refused`` and never deleted. Use ``migrate_legacy_nested``
    to relocate them by hand.

    Two additional guards, both opt-in via keyword so existing callers are
    unaffected:

    * ``protected_paths`` — entries matching (per :func:`is_worktree_protected`)
      are refused and the refusal is logged (never silent). Pass
      :func:`protected_worktree_paths` for the systemd-derived set.
    * ``check_git_safety`` — when true, a locked/dirty/unpushed worktree
      (per :func:`git_worktree_status`, or ``git_status_provider`` for
      test injection) is refused and logged, even if TTL/count would
      otherwise select it.

    Returns a :class:`ReapSummary`. The caller decides whether to
    actually delete (the CLI pairs this with ``--dry-run``).
    """
    summary = ReapSummary(root=root)
    primary = Path(primary_checkout).expanduser().resolve()
    if not root.exists():
        return summary

    if ttl_seconds is None and max_count is None:
        return summary

    protected_list = list(protected_paths)

    candidates: list[tuple[Path, float]] = []
    for child in sorted(root.iterdir()):
        if not child.is_dir():
            continue
        if _is_inside(child, primary):
            summary.refused.append(ReapedWorktree(path=child, reason="nested-legacy"))
            continue
        if protected_list and is_worktree_protected(child, protected_list):
            reason = "protected: referenced by an enabled systemd unit"
            summary.refused.append(ReapedWorktree(path=child, reason=reason))
            logger.warning("refusing to prune worktree %s: %s", child, reason)
            continue
        if check_git_safety:
            safe, why = is_worktree_safe_to_prune(
                child, status=git_status_provider(child)
            )
            if not safe:
                reason = f"unsafe: {why}"
                summary.refused.append(ReapedWorktree(path=child, reason=reason))
                logger.warning("refusing to prune worktree %s: %s", child, why)
                continue
        try:
            mtime = child.stat().st_mtime
        except OSError:
            continue
        candidates.append((child, mtime))

    # Sort oldest first.
    candidates.sort(key=lambda t: t[1])
    now_ts = float(now if now is not None else time.time())

    # Pass 1 — TTL.
    if ttl_seconds is not None:
        for path, mtime in list(candidates):
            age = now_ts - mtime
            if age > ttl_seconds:
                summary.removed.append(ReapedWorktree(path=path, reason="ttl"))
                candidates.remove((path, mtime))

    # Pass 2 — count cap (oldest first).
    if max_count is not None:
        while len(candidates) > max_count:
            path, _ = candidates.pop(0)
            summary.removed.append(ReapedWorktree(path=path, reason="count"))

    summary.survivors = [p for p, _ in candidates]
    return summary


def apply_reap(summary: ReapSummary, *, dry_run: bool = False) -> ReapSummary:
    """Actually delete the entries in ``summary.removed``.

    Separated from :func:`reap_stale_worktrees` so the CLI can run the
    plan in dry-run mode without touching the filesystem.
    """
    if dry_run:
        return summary
    for entry in list(summary.removed):
        try:
            shutil.rmtree(entry.path, ignore_errors=False)
            # Drop removed entries that were successfully deleted; keep
            # those that survived so the CLI can flag them.
        except OSError as exc:
            if exc.errno != errno.ENOENT:
                raise
    return summary


# ---------------------------------------------------------------------------
# Internals
# ---------------------------------------------------------------------------


def _repo_slug(primary: Path) -> str:
    """Slug for ``primary`` — basename, falling back to 'workspace'."""
    name = primary.name.strip() or "workspace"
    return name


def _resolve_agent_root(
    repo_slug: str,
    agent_id: str,
    *,
    override: Optional[Path],
) -> Path:
    if override is not None:
        return (Path(override) / agent_id).expanduser().resolve()
    env = os.environ.get(ENV_AGENT_WORKTREES_ROOT, "").strip()
    if env:
        return (Path(env) / agent_id).expanduser().resolve()
    return (Path.home() / ".worktrees" / repo_slug / agent_id).expanduser().resolve()


def _is_inside(child: Path, parent: Path) -> bool:
    try:
        return child.resolve().is_relative_to(parent)
    except AttributeError:
        resolved = str(child.resolve())
        return resolved.startswith(str(parent) + os.sep)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="df-reap-worktrees",
        description=(
            "Reap stale agent worktrees outside the primary checkout. "
            "Refuses to delete directories that live INSIDE the primary checkout."
        ),
    )
    p.add_argument(
        "--primary-checkout",
        required=True,
        type=Path,
        help="Absolute path to the primary checkout whose agent worktrees will be reaped.",
    )
    p.add_argument(
        "--root",
        type=Path,
        default=None,
        help=(
            "Directory of agent worktrees. Defaults to "
            "$DARK_FACTORY_AGENT_WORKTREES or ~/.worktrees/<repo>/."
        ),
    )
    p.add_argument(
        "--ttl-seconds",
        type=int,
        default=None,
        help="Remove entries older than this many seconds.",
    )
    p.add_argument(
        "--max-count",
        type=int,
        default=None,
        help="Cap the number of surviving entries (oldest removed first).",
    )
    p.add_argument(
        "--dry-run",
        action="store_true",
        help="Report what would be removed without deleting anything.",
    )
    p.add_argument(
        "--apply",
        action="store_true",
        help="Actually delete. Required to mutate the filesystem.",
    )
    p.add_argument(
        "--no-systemd-protect",
        action="store_true",
        help=(
            "Opt out of the systemd protect-list guard. By default the CLI "
            "derives paths referenced by ENABLED systemd --user units and "
            "refuses to prune any worktree in that set (this is what keeps "
            "the merge-guard's WorkingDirectory worktree alive). Pass this "
            "flag only when you have a verified reason to bypass."
        ),
    )
    p.add_argument(
        "--no-git-safety",
        action="store_true",
        help=(
            "Opt out of the git safety guard. By default the CLI refuses to "
            "prune a worktree that is locked, has uncommitted changes, or "
            "has unpushed commits. Pass this flag only when you have a "
            "verified reason to bypass."
        ),
    )
    return p


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = _build_arg_parser()
    args = parser.parse_args(argv)

    primary = args.primary_checkout.expanduser().resolve()
    repo_slug = _repo_slug(primary)
    if args.root is None:
        env_root = os.environ.get(ENV_AGENT_WORKTREES_ROOT, "").strip()
        root = (
            (Path(env_root) if env_root else Path.home() / ".worktrees" / repo_slug)
            .expanduser()
            .resolve()
        )
    else:
        root = args.root.expanduser().resolve()

    ttl_env = os.environ.get(ENV_AGENT_WORKTREES_TTL)
    if args.ttl_seconds is None and ttl_env:
        try:
            ttl_seconds = int(float(ttl_env) * 86400)
        except ValueError:
            ttl_seconds = None
    else:
        ttl_seconds = args.ttl_seconds

    max_env = os.environ.get(ENV_AGENT_WORKTREES_MAX)
    if args.max_count is None and max_env:
        try:
            max_count = int(max_env)
        except ValueError:
            max_count = None
    else:
        max_count = args.max_count

    if not args.dry_run and not args.apply:
        print(
            "refusing to run without --apply or --dry-run; pass one explicitly.",
            file=sys.stderr,
        )
        return 2

    # Defaults: BOTH guards are ON. The CLI is the safe-by-default surface;
    # the systemd protect-list keeps the merge-guard worktree alive, and
    # the git safety guard refuses to prune locked/dirty/unpushed worktrees.
    # Operators who need to bypass can pass --no-systemd-protect and/or
    # --no-git-safety (bead jleechan-6i73).
    if args.no_systemd_protect:
        protected_paths: tuple[Path, ...] = ()
    else:
        protected_paths = tuple(protected_worktree_paths())
    check_git_safety = not args.no_git_safety

    summary = reap_stale_worktrees(
        root=root,
        primary_checkout=primary,
        ttl_seconds=ttl_seconds,
        max_count=max_count,
        protected_paths=protected_paths,
        check_git_safety=check_git_safety,
    )
    if args.apply:
        summary = apply_reap(summary, dry_run=False)

    for line in summary.human_lines():
        print(line)
    if args.dry_run and summary.removed:
        print("(dry-run; nothing was deleted)")
    return 0


if __name__ == "__main__":  # pragma: no cover - CLI entry
    raise SystemExit(main())
