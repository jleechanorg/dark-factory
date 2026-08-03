"""Resolve Dark Factory install paths (DARK_FACTORY_HOME)."""

from __future__ import annotations

import os
import pathlib


_PIPELINE_SHORT_NAMES = {
    "two_node": "pipelines/slim/two_node.dot",
    "gates": "pipelines/factory/gates.dot",
    "hello": "pipelines/factory/hello.dot",
    "pr_gates": "pipelines/factory/pr_gates.dot",
    "minimal_feature": "pipelines/slim/minimal_feature.dot",
    "minimal_pr": "pipelines/slim/minimal_pr.dot",
    "review_slim": "benchmarks/attractor-spec-review/pipelines/review_slim.dot",
    "review_full": "benchmarks/attractor-spec-review/pipelines/review_full.dot",
    "spec_gen": "pipelines/slim/spec_gen.dot",
    "bug_fix": "pipelines/bug_fix.dot",
    "level5_feature": "pipelines/factory/level5_feature.dot",
}


def factory_home() -> pathlib.Path | None:
    raw = os.environ.get("DARK_FACTORY_HOME", "").strip()
    if not raw:
        return None
    return pathlib.Path(raw).expanduser().resolve()


def resolve_factory_path(path: pathlib.Path) -> pathlib.Path:
    """Resolve `path` against cwd, then DARK_FACTORY_HOME when relative."""
    candidate = path.expanduser()
    if candidate.is_absolute():
        return candidate
    if candidate.exists():
        return candidate.resolve()
    home = factory_home()
    if home is not None:
        under_home = (home / candidate).resolve()
        if under_home.exists():
            return under_home
        for sub in ["pipelines/slim", "pipelines/factory", "pipelines"]:
            under_sub = (home / sub / candidate).resolve()
            if under_sub.exists():
                return under_sub
    return candidate.resolve()



def resolve_pipeline_path(
    name: str | os.PathLike[str],
    workdir: pathlib.Path | None = None,
) -> pathlib.Path:
    """Resolve a `--pipeline` argument to a concrete `.dot` path.

    Resolution order (first match wins):

    1. Absolute path → returned as resolved.
    2. A documented extensionless short name maps to its canonical factory
       pipeline before target-repository lookups. This makes names such as
       ``minimal_feature`` deterministic even when a target repo contains a
       colliding file.
    3. ``<workdir>/<name>`` if it exists (operator is already in the target
       repo and the file lives at a normal relative path).
    4. ``<workdir>/dark-factory/pipelines/<name>`` **when `name` is a bare
       filename** (no path separators) — this is the target-repo
       `dark-factory/pipelines/*.dot` convention that lets a downstream repo
       ship its own repo-specific graphs (e.g. ones that hardcode that
       repo's slash commands) without leaking them into dark-factory itself.
    5. Delegate to :func:`resolve_factory_path` (cwd → ``$DARK_FACTORY_HOME``).

    Parameters
    ----------
    name:
        The value passed to ``--pipeline`` — either a path (with separators
        or a ``.dot`` extension) or a bare filename. Known extensionless
        aliases resolve canonically first; other bare filenames are matched
        against the target-repo subdir convention
        before falling through to the factory home.
    workdir:
        Target repository workdir (``--workdir`` / cwd). When ``None`` the
        current process cwd is used.
    """
    candidate = pathlib.Path(os.fspath(name)).expanduser()
    base = (
        pathlib.Path(workdir).expanduser().resolve()
        if workdir is not None
        else pathlib.Path.cwd()
    )

    if candidate.is_absolute():
        return candidate.resolve()

    short_path = _PIPELINE_SHORT_NAMES.get(str(candidate))
    if short_path is not None:
        return resolve_factory_path(pathlib.Path(short_path))

    local = base / candidate
    if local.exists():
        return local.resolve()

    # Target-repo subdir convention: only when the user passed a bare filename.
    # A path like `pipelines/factory/gates.dot` is intentionally not rewritten
    # — the subdir convention is for short names that the operator expects to
    # resolve in their own repo, not the factory home.
    if "/" not in str(candidate) and "\\" not in str(candidate):
        subdir_candidate = base / "dark-factory" / "pipelines" / candidate
        if subdir_candidate.exists():
            return subdir_candidate.resolve()

    return resolve_factory_path(candidate)
