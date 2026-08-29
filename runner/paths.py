"""Resolve Dark Factory install paths (DARK_FACTORY_HOME)."""

from __future__ import annotations

import os
import pathlib


SHORT_PIPELINE_ALIASES: dict[str, str] = {
    "two_node": "pipelines/slim/two_node.dot",
    "ready": "pipelines/slim/ready.dot",
    "gates": "pipelines/factory/gates.dot",
    "hello": "pipelines/factory/hello.dot",
    "pr_gates": "pipelines/factory/pr_gates.dot",
    "minimal_feature": "pipelines/slim/minimal_feature.dot",
    "minimal_pr": "pipelines/slim/minimal_pr.dot",
    "minimal_research": "pipelines/slim/minimal_research.dot",
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
    if candidate.suffix != ".dot" and candidate.with_suffix(".dot").exists():
        return candidate.with_suffix(".dot").resolve()
    home = factory_home()
    roots = [home] if home is not None else [pathlib.Path.cwd(), pathlib.Path(__file__).resolve().parent.parent]
    for root in roots:
        under_root = (root / candidate).resolve()
        if under_root.exists():
            return under_root
        if candidate.suffix != ".dot":
            under_dot = (root / candidate.with_suffix(".dot")).resolve()
            if under_dot.exists():
                return under_dot
        for sub in [
            "pipelines/slim",
            "pipelines/factory",
            "pipelines",
            "benchmarks/attractor-spec-review/pipelines",
        ]:
            under_sub = (root / sub / candidate).resolve()
            if under_sub.exists():
                return under_sub
            if candidate.suffix != ".dot":
                under_sub_dot = (root / sub / candidate.with_suffix(".dot")).resolve()
                if under_sub_dot.exists():
                    return under_sub_dot
    return candidate.resolve()


def resolve_pipeline_path(
    name: str | os.PathLike[str],
    workdir: pathlib.Path | None = None,
) -> pathlib.Path:
    """Resolve a `--pipeline` argument to a concrete `.dot` path.

    Resolution order (first match wins):

    1. Absolute path → returned as resolved.
    2. ``<workdir>/<name>`` (and ``<name>.dot``) if it exists (operator is
       already in the target repo and the file lives at a normal relative path).
    3. ``<workdir>/dark-factory/pipelines/<name>`` (and ``<name>.dot``)
       **when `name` is a bare filename** (no path separators) — this is the
       target-repo `dark-factory/pipelines/*.dot` convention that lets a
       downstream repo ship its own repo-specific graphs without leaking them.
    4. Documented short pipeline aliases (e.g. ``ready`` → ``pipelines/slim/ready.dot``).
    5. Delegate to :func:`resolve_factory_path` (cwd → ``$DARK_FACTORY_HOME``).

    Parameters
    ----------
    name:
        The value passed to ``--pipeline`` — either a path (with separators
        or a ``.dot`` extension) or a bare filename. Bare filenames are
        matched against the target-repo subdir convention **before** falling
        through to the factory home.
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

    local = base / candidate
    if local.exists():
        return local.resolve()
    if candidate.suffix != ".dot" and (base / candidate.with_suffix(".dot")).exists():
        return (base / candidate.with_suffix(".dot")).resolve()

    # Target-repo subdir convention: only when the user passed a bare filename.
    # A path like `pipelines/factory/gates.dot` is intentionally not rewritten
    # — the subdir convention is for short names that the operator expects to
    # resolve in their own repo, not the factory home.
    if "/" not in str(candidate) and "\\" not in str(candidate):
        subdir = base / "dark-factory" / "pipelines"
        subdir_candidate = subdir / candidate
        if subdir_candidate.exists():
            return subdir_candidate.resolve()
        if candidate.suffix != ".dot" and (subdir / candidate.with_suffix(".dot")).exists():
            return (subdir / candidate.with_suffix(".dot")).resolve()

    # Short aliases
    raw_key = candidate.stem if candidate.suffix == ".dot" else str(candidate)
    if raw_key in SHORT_PIPELINE_ALIASES:
        alias_rel = pathlib.Path(SHORT_PIPELINE_ALIASES[raw_key])
        resolved_alias = resolve_factory_path(alias_rel)
        if resolved_alias.exists():
            return resolved_alias

    return resolve_factory_path(candidate)
