"""Branch context isolation for parallel fan-out.

Splits out from `runner/engine.py` (see `docs/refactor/file-ownership-map.engine.md`).
Holds the read-only branch-type gating and the per-branch workdir tempdir allocator.
"""

from __future__ import annotations

import pathlib
import tempfile
import warnings
from typing import TYPE_CHECKING

from .handlers import Context

if TYPE_CHECKING:
    pass


# Branch-start node types that must run against the REAL parent workdir.
# Reviewer gates are read-only and SHA-bound (they review the repo at
# ctx.workdir's HEAD); holdout_eval resolves the implementation path from
# ctx.workdir. Handing these an empty isolation tempdir breaks them — the
# gate can't resolve a HEAD SHA, find .claude/commands/, or see the diff.
# Only file-WRITING branches (codergen, tool) need tempdir isolation.
#
# NOTE: gate_red / gate_green are intentionally excluded. They run pytest
# in ctx.workdir and write .pyc / .pytest_cache / coverage artifacts; two
# parallel branches sharing the parent workdir would race on those writes.
# If you ever need to parallelize them, give them their own per-branch
# tempdir and have the gate bind to ctx.workdir's HEAD SHA explicitly.
_READ_ONLY_BRANCH_TYPES = frozenset({
    "gate_es", "gate_er", "gate_code_standards", "gate_slash",
    "holdout_eval", "parallel_reviewer",
})


def _clone_context(ctx: Context) -> Context:
    return Context(
        goal=ctx.goal,
        workdir=ctx.workdir,
        state=dict(ctx.state),
        history=[dict(entry) for entry in ctx.history],
        backend=ctx.backend,
        cxdb_path=ctx.cxdb_path,
        run_id=ctx.run_id,
        event_log_path=getattr(ctx, "event_log_path", None),
        perf_log_root=getattr(ctx, "perf_log_root", None),
        git_ctx=getattr(ctx, "git_ctx", None),
        perf_run=getattr(ctx, "perf_run", None),
        last_completed_seq=getattr(ctx, "last_completed_seq", 0),
        _operator_trust=dict(getattr(ctx, "_operator_trust", {})),
    )


def _branch_context(ctx: Context, branch_name: str, node_type: str = "") -> Context:
    """Clone ctx and assign a unique per-branch workdir subdirectory.

    File-writing backends (claude, codex, agy) spawn subprocesses with
    cwd=ctx.workdir; without isolation concurrent branches would race on
    shared files.  Each branch gets its own tempdir under the parent workdir
    so their file operations are independent.  If the parent workdir is not a
    valid directory (e.g. None or a non-existent path) fall back to a
    system-managed tmp dir.
    Branches that start at a read-only node type (reviewer gates,
    holdout_eval) skip isolation and keep the parent workdir — they need the
    real repo, and being read-only they cannot race each other.
    """
    cloned = _clone_context(ctx)
    if node_type in _READ_ONLY_BRANCH_TYPES:
        return cloned
    parent = pathlib.Path(ctx.workdir) if ctx.workdir else None
    try:
        base = parent if (parent and parent.is_dir()) else None
        branch_dir = pathlib.Path(tempfile.mkdtemp(prefix=f"branch_{branch_name}_", dir=base))
        cloned.workdir = branch_dir
    except OSError as exc:
        warnings.warn(
            f"_branch_context: mkdtemp failed for '{branch_name}', branch isolation disabled: {exc}",
            RuntimeWarning,
            stacklevel=2,
        )
    return cloned
