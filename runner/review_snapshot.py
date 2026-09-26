"""Isolated snapshot materialization for the fresh, verdict-gated reviewer.

Implements design "Runner changes" item 6
(``docs/superpowers/specs/2026-09-01-factory-two-node-redesign-design.md``):
the fresh cold reviewer must never run against the live coder workdir. It
runs against a detached ``git worktree`` checked out at the pin the runner
minted after the worker's last visit (``ctx.state["target"]``, D3/D8a), with
repository agent-config files mechanically quarantined so they are reviewable
as data but never loaded as CLI instructions.

Fail-closed by design: every public function either returns a materialized
``ReviewSnapshot`` / ``True`` or raises ``ReviewSnapshotError`` — callers
must abort the reviewer visit as a failure, never fall back to the live
workdir.
"""

from __future__ import annotations

import pathlib
import subprocess
import uuid
from dataclasses import dataclass

from . import target_locator
from ._git import _git_rev_parse

# Only schemes that resolve to a local git repo root + a single commit pin
# can be materialized as a detached worktree. `git-worktree://` pins a
# dirty-tree fingerprint (not a git ref) and `file://` pins a raw-bytes
# digest (not a repo) — out of scope for v1 (design "Out of scope": target-mode
# write semantics land as a follow-up).
_SNAPSHOTTABLE_SCHEMES = frozenset({"git-range", "git-commit"})

# Repository agent-config paths quarantined inside the snapshot so a reviewer
# CLI never loads them as instructions, regardless of CLI doc-loading flags
# (design item 6). Renamed in place: `<name>` -> `<name>.factory-quarantined`.
_QUARANTINE_NAMES = frozenset({"AGENTS.md", "CLAUDE.md", ".agents", ".claude"})

# Directory components never descended into while quarantining (git internals
# and any snapshot the walk starts from should not be treated as data).
_QUARANTINE_SKIP_DIRS = frozenset({".git"})


class ReviewSnapshotError(Exception):
    """Base for all fresh-reviewer snapshot failures (fail closed)."""


@dataclass(frozen=True)
class ReviewSnapshot:
    """A materialized, pin-verified detached worktree for the fresh reviewer."""

    path: pathlib.Path
    repo_root: pathlib.Path
    pin: str


def _snapshot_source(locator: "target_locator.Locator") -> tuple[pathlib.Path, str]:
    """Return ``(repo_root, commit_sha)`` for a v1 locator, or raise.

    ``git-range://<path>@<base>..<head>`` snapshots at ``head`` (the pin the
    reviewer must see includes every committed worker edit up to the current
    visit). ``git-commit://<path>@<sha>`` snapshots at ``sha`` directly.
    """
    if locator.scheme not in _SNAPSHOTTABLE_SCHEMES:
        raise ReviewSnapshotError(
            f"target scheme {locator.scheme!r} has no git-resolvable review "
            "snapshot pin (only git-range/git-commit are snapshottable in v1)"
        )
    repo_root = pathlib.Path(locator.body)
    if locator.scheme == "git-range":
        pin = (locator.pin or "").rsplit("..", 1)[-1]
    else:
        pin = locator.pin or ""
    if not pin:
        raise ReviewSnapshotError(f"target locator has no resolvable pin: {locator.canonical}")
    return repo_root, pin


def _default_snapshot_root() -> pathlib.Path:
    root = pathlib.Path.home() / ".dark-factory" / "review-snapshots"
    root.mkdir(parents=True, exist_ok=True)
    return root


def _quarantine_agent_config(snapshot_root: pathlib.Path) -> list[pathlib.Path]:
    """Rename every ``AGENTS.md``/``CLAUDE.md``/``.agents``/``.claude`` entry
    under ``snapshot_root`` to ``<name>.factory-quarantined``, recursively
    (repo-config files can live in nested directories, not only at the
    root). Returns the list of quarantined paths (post-rename)."""
    quarantined: list[pathlib.Path] = []
    stack = [snapshot_root]
    while stack:
        current = stack.pop()
        try:
            entries = list(current.iterdir())
        except OSError:
            continue
        for entry in entries:
            if entry.name in _QUARANTINE_SKIP_DIRS:
                continue
            if entry.name in _QUARANTINE_NAMES:
                target = entry.with_name(entry.name + ".factory-quarantined")
                entry.rename(target)
                quarantined.append(target)
                continue
            if entry.is_dir() and not entry.is_symlink():
                stack.append(entry)
    return quarantined


def create_review_snapshot(
    target: str,
    *,
    snapshot_root: "pathlib.Path | None" = None,
) -> ReviewSnapshot:
    """Materialize an isolated, pin-verified snapshot for the fresh reviewer.

    Parses ``target`` (``ctx.state["target"]``, the runner-minted D3/D8a
    locator), resolves it to a local repo root + commit pin, checks out a
    detached ``git worktree`` at that pin under a private snapshot root, and
    mechanically quarantines repository agent-config files. Raises
    ``ReviewSnapshotError`` on any failure; the snapshot (if partially
    created) is best-effort cleaned up before raising.
    """
    if not target or not str(target).strip():
        raise ReviewSnapshotError("no review target has been minted")
    try:
        locator = target_locator.parse(target)
    except target_locator.TargetLocatorError as exc:
        raise ReviewSnapshotError(f"could not parse review target: {exc}") from exc

    repo_root, pin = _snapshot_source(locator)
    if not repo_root.is_absolute() or ".." in repo_root.parts or not repo_root.is_dir():
        raise ReviewSnapshotError(f"review target repo root is not a real directory: {repo_root}")

    root = snapshot_root if snapshot_root is not None else _default_snapshot_root()
    root.mkdir(parents=True, exist_ok=True)
    snapshot_path = root / f"review-{uuid.uuid4().hex}"
    if snapshot_path.exists() or snapshot_path.is_symlink():
        raise ReviewSnapshotError(f"review snapshot path already exists: {snapshot_path}")

    add = subprocess.run(
        ["git", "-C", str(repo_root), "worktree", "add", "--detach", str(snapshot_path), pin],
        capture_output=True, text=True, timeout=60, check=False,
    )
    if add.returncode != 0:
        raise ReviewSnapshotError(add.stderr.strip() or "could not create review snapshot worktree")

    snapshot = ReviewSnapshot(path=snapshot_path, repo_root=repo_root, pin=pin.lower())
    if not verify_review_snapshot_pin(snapshot):
        cleanup_review_snapshot(snapshot)
        raise ReviewSnapshotError(
            f"review snapshot pin mismatch immediately after creation (TOCTOU): expected {pin}"
        )

    _quarantine_agent_config(snapshot_path)
    return snapshot


def verify_review_snapshot_pin(snapshot: ReviewSnapshot) -> bool:
    """TOCTOU check (design item 6): confirm the snapshot's checked-out HEAD
    still equals the pin the runner minted. Call this immediately before
    launching the reviewer, not only at snapshot creation time."""
    observed = _git_rev_parse(snapshot.path, "HEAD^{commit}")
    return observed is not None and observed.lower() == snapshot.pin.lower()


def cleanup_review_snapshot(snapshot: ReviewSnapshot) -> bool:
    """Best-effort ``git worktree remove`` of the snapshot. Returns True on
    success; failure is never raised (D "cleanup ... best-effort with
    failure recorded" — callers record the False return in Result metadata)."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(snapshot.repo_root), "worktree", "remove", "--force", str(snapshot.path)],
            capture_output=True, text=True, timeout=30, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    if proc.returncode == 0:
        return True
    # The worktree metadata may already be gone (e.g. directory removed out
    # of band); prune stale registrations best-effort and report failure only
    # if the snapshot directory is still actually there.
    subprocess.run(
        ["git", "-C", str(snapshot.repo_root), "worktree", "prune"],
        capture_output=True, text=True, timeout=30, check=False,
    )
    return not snapshot.path.exists()
