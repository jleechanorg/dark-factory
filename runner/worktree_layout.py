"""Canonical agent worktree layout + reaper (bead jleechan-jw4c).

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
import os
import shutil
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, Optional, Sequence


ENV_AGENT_WORKTREES_ROOT = "DARK_FACTORY_AGENT_WORKTREES"
ENV_AGENT_WORKTREES_TTL = "DARK_FACTORY_AGENT_WORKTREES_TTL_DAYS"
ENV_AGENT_WORKTREES_MAX = "DARK_FACTORY_AGENT_WORKTREES_MAX_COUNT"


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
# Reaper
# ---------------------------------------------------------------------------


def reap_stale_worktrees(
    *,
    root: Path,
    primary_checkout: Path,
    ttl_seconds: Optional[int],
    max_count: Optional[int],
    now: Optional[float] = None,
) -> ReapSummary:
    """Prune stale entries under ``root``.

    Forward-only: directories that resolve INSIDE ``primary_checkout``
    are recorded as ``refused`` and never deleted. Use ``migrate_legacy_nested``
    to relocate them by hand.

    Returns a :class:`ReapSummary`. The caller decides whether to
    actually delete (the CLI pairs this with ``--dry-run``).
    """
    summary = ReapSummary(root=root)
    primary = Path(primary_checkout).expanduser().resolve()
    if not root.exists():
        return summary

    if ttl_seconds is None and max_count is None:
        return summary

    candidates: list[tuple[Path, float]] = []
    for child in sorted(root.iterdir()):
        if not child.is_dir():
            continue
        if _is_inside(child, primary):
            summary.refused.append(ReapedWorktree(path=child, reason="nested-legacy"))
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

    summary = reap_stale_worktrees(
        root=root,
        primary_checkout=primary,
        ttl_seconds=ttl_seconds,
        max_count=max_count,
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
