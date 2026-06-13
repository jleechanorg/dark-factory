"""Filesystem-safe slug for repo, branch, and pipeline-node names.

Goal
----
The runner and the evidence bundle materialiser both need to turn
arbitrary user-controlled strings (branch names like
``feat/my-feature``, node names like ``holdout_eval-2``) into names
that are safe to embed in a filesystem path. Two near-identical
helpers previously lived in ``runner.perf_log._safe_slug`` and
``runner.evidence._safe_filename`` — both ran the same regex
(``[^A-Za-z0-9._-]+`` → ``_``), truncated to 64 characters, and
differed only in the fallback string used when the result was
empty. This module is the single source of truth for that pattern.

API
---
- :func:`safe_slug` — return a filesystem-safe slug derived from an
  arbitrary input string, with a caller-supplied fallback for the
  empty-input case.

The regex is compiled once at module load. Truncation is fixed at
64 characters — the load-bearing invariant for log readers that
split paths on the slug boundary. Callers that need a different
limit should add a ``max_len`` parameter rather than mutating the
module-level constant.

Example
-------
::

    from runner._slug import safe_slug

    safe_slug("feat/my-feature")
    # -> "feat_my-feature"

    safe_slug("holdout_eval-2", fallback="node")
    # -> "holdout_eval-2"

    safe_slug("")
    # -> "unknown"  (default fallback)
"""

from __future__ import annotations

import re

# Module-level constants are the canonical pattern across
# runner/_git.py, runner/_run_id.py, etc. They are intentionally
# private (leading underscore) but accessible to tests.
_SLUG_RE = re.compile(r"[^A-Za-z0-9._-]+")
_MAX_LEN = 64


def safe_slug(name: str, *, fallback: str = "unknown") -> str:
    """Return a filesystem-safe slug derived from ``name``.

    Any run of characters outside ``[A-Za-z0-9._-]`` collapses to a
    single underscore. The result is truncated to 64 characters. If
    the sanitised result is empty, ``fallback`` is returned instead
    so callers never need to handle the empty-string case.

    Parameters
    ----------
    name:
        Arbitrary string to slugify — typically a branch name, repo
        basename, or pipeline node name. May be empty.
    fallback:
        Returned when the slug is empty after sanitisation. Callers
        pick a context-appropriate value (``"unknown"`` for repo
        identity, ``"node"`` for pipeline node filenames).

    Returns
    -------
    A non-empty string safe to embed in a filesystem path.

    Notes
    -----
    The fallback only fires for the literal empty-input case. An
    input of all-unsafe characters (e.g. ``"///"``) collapses to
    a single ``"_"`` which is truthy and therefore returned as-is.
    This is a subtle pre-existing behaviour both originals shared
    and the consolidated function preserves intentionally.
    """
    return _SLUG_RE.sub("_", name)[:_MAX_LEN] or fallback
