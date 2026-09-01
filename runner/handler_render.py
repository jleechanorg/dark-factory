"""Prompt template resolution (``@path`` → rendered text).

Owns:
  * `_render_prompt` — resolve ``@path``, enforce workdir-relative, holdout-deny,
    substitute ``${goal}`` / ``${state.<key>}`` / ``${diff}` / ``${lint_findings}``.
    Mirrors ``runner.handlers._render_prompt`` so ``tests/test_prompt_pinning.py``
    can pin the same resolution order.

Substitutions (in order):
  * ``${goal}``     → the run-level goal text
  * ``${state.<key>}`` → any ``ctx.state`` key, e.g. ``${state._last_output}``
  * ``${diff}``     → the most recent codergen's ``git diff`` (G4), captured
    automatically by ``_codergen`` on the success path. Defaults to
    ``"(no diff captured)"`` when no codergen has run yet (or when the
    capture silently failed because the workdir is not a git repo).
  * ``${lint_findings}`` → Markdown table of engine-computed lint findings
    (F5, jleechan-zba). Computed once per render via
    ``runner/pre_review_lint.py::lint_findings(ctx.workdir)``. Cached in
    ``ctx.state["_lint_findings"]`` so a prompt that references the
    placeholder twice doesn't re-scan. Defaults to ``"(none)"`` when the
    scan returns no findings.

The ``runner.handlers._holdout_denied_paths`` symbol is looked up via the
shim at runtime (lazy import inside ``_render_prompt``) so existing test
monkeypatching via ``monkeypatch.setattr("runner.handlers._holdout_denied_paths", ...)``
keeps working. The shim is NOT imported at module top to avoid a load-time
cycle: ``runner.handlers`` imports this file, and a partial re-import would
re-enter the shim before ``_holdout_denied_paths`` was bound.
"""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
from typing import TYPE_CHECKING

from .handler_core import _serialize_state_value
from .pre_review_lint import findings_to_markdown, findings_to_json, lint_findings


_FRESH_REVIEW_PROMPT_REF = "prompts/slim/fresh_review.md"


def _is_fresh_review_node(node: "Node", backend: str) -> bool:
    """Identify the default fully-tooled reviewer that owns the pinned prompt."""
    return (
        backend == "codex"
        and str(node.attrs.get("class", "")).strip().lower() == "review"
        and str(node.attrs.get("verdict_gate", "false")).strip().lower()
        in {"true", "1", "yes", "on"}
        and str(node.attrs.get("fresh_session", "false")).strip().lower()
        in {"true", "1", "yes", "on"}
        and node.prompt_ref == _FRESH_REVIEW_PROMPT_REF
    )


def _factory_owned_fresh_review_source() -> tuple[pathlib.Path, bytes] | None:
    """Read the installed Factory copy of the default review authority.

    The module location is the only source because it travels with the
    installed release. The candidate must be a regular, non-symlink file
    beneath that release; a missing or redirected authority fails closed.
    Opening and reading the descriptor here keeps validation, rendering, and
    provenance on the same source snapshot.
    """
    import stat

    release_root = pathlib.Path(__file__).resolve().parents[1]
    candidate = release_root / _FRESH_REVIEW_PROMPT_REF
    nofollow = getattr(os, "O_NOFOLLOW", None)
    if nofollow is None:
        return None
    try:
        candidate.relative_to(release_root)
        if candidate.is_symlink() or not candidate.is_file():
            return None
    except (OSError, ValueError):
        return None

    fd = None
    try:
        fd = os.open(candidate, os.O_RDONLY | nofollow)
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            return None
        import fcntl

        getpath = getattr(fcntl, "F_GETPATH", None)
        if getpath is not None:
            raw_path = fcntl.fcntl(fd, getpath, b"\0" * 1024)
            fd_source = pathlib.Path(raw_path.split(b"\0", 1)[0].decode())
        else:
            fd_source = pathlib.Path(os.path.realpath(f"/proc/self/fd/{fd}"))
        fd_source = fd_source.resolve(strict=True)
        fd_source.relative_to(release_root)
        with os.fdopen(os.dup(fd), "rb") as source_file:
            source_bytes = source_file.read()
        return fd_source, source_bytes
    except (OSError, ValueError):
        return None
    finally:
        if fd is not None:
            os.close(fd)


def _fresh_review_prompt_cache_key(node: "Node", backend: str, ctx: "Context") -> tuple:
    """Identify one rendered reviewer attempt for provenance caching."""
    return (
        node.name,
        backend,
        str(ctx.goal),
        getattr(ctx, "_df_current_seq", None),
        getattr(ctx, "_df_current_attempt", None),
    )


def _cached_fresh_review_prompt(node: "Node", backend: str, ctx: "Context") -> dict | None:
    cache = getattr(ctx, "_fresh_review_prompt_cache", None)
    if not isinstance(cache, dict):
        return None
    if cache.get("key") != _fresh_review_prompt_cache_key(node, backend, ctx):
        return None
    return cache


def _fresh_review_prompt_source(
    node: "Node", backend: str
) -> tuple[pathlib.Path, bytes] | None:
    """Load the canonical review prompt when ``node`` is the default reviewer."""
    if not _is_fresh_review_node(node, backend):
        return None
    return _factory_owned_fresh_review_source()


def _fresh_review_prompt_metadata(
    node: "Node", backend: str, rendered_prompt: str, ctx: "Context" | None = None
) -> dict[str, str]:
    """Return provenance hashes for a Factory-owned fresh-review invocation."""
    if not _is_fresh_review_node(node, backend):
        return {}
    cache = _cached_fresh_review_prompt(node, backend, ctx) if ctx is not None else None
    if cache is None or cache.get("rendered") != rendered_prompt:
        return {
            "review_prompt_source": "factory://prompts/slim/fresh_review.md",
            "review_prompt_contract_sha256": "",
            "review_prompt_rendered_sha256": hashlib.sha256(
                rendered_prompt.encode("utf-8")
            ).hexdigest(),
            "review_prompt_error": "fresh-review prompt provenance is unavailable",
        }
    source_text = cache.get("source_text")
    source_path = cache.get("source_path")
    source_bytes = cache.get("source_bytes")
    if (
        not isinstance(source_text, str)
        or not isinstance(source_path, str)
        or not source_path
        or not isinstance(source_bytes, bytes)
        or not source_bytes
    ):
        return {
            "review_prompt_source": "factory://prompts/slim/fresh_review.md",
            "review_prompt_contract_sha256": "",
            "review_prompt_rendered_sha256": hashlib.sha256(
                rendered_prompt.encode("utf-8")
            ).hexdigest(),
            "review_prompt_error": "fresh-review prompt provenance is unavailable",
        }
    return {
        "review_prompt_source": "factory://prompts/slim/fresh_review.md",
        "review_prompt_contract_sha256": hashlib.sha256(
            source_bytes
        ).hexdigest(),
        "review_prompt_rendered_sha256": hashlib.sha256(
            rendered_prompt.encode("utf-8")
        ).hexdigest(),
    }


if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _resolve_lint_findings(ctx: "Context") -> list[dict]:
    """Return the cached lint findings, computing on first call."""
    cached = ctx.state.get("_lint_findings")
    if cached is not None:
        return json.loads(cached)
    findings = lint_findings(ctx.workdir)
    ctx.state["_lint_findings"] = findings_to_json(findings)
    return findings


def _substitute_placeholders(text: str, ctx: "Context") -> str:
    """Apply ``${goal}`` / ``${state.<key>}`` / ``${diff}`` / ``${lint_findings}`` substitutions.

    ``${diff}`` resolves to ``ctx.state["_last_diff"]`` if a successful
    codergen stashed one, else the placeholder ``"(no diff captured)"`` so
    reviewer prompts never render an empty cell where the diff should be.

    ``${lint_findings}`` resolves to a Markdown table of engine-computed
    findings (F5, jleechan-zba). Findings are computed once per render and
    cached in ``ctx.state["_lint_findings"]`` so multiple substitutions
    don't re-scan the workdir.
    """
    text = text.replace("${goal}", ctx.goal)
    for k, v in ctx.state.items():
        placeholder = "${state." + k + "}"
        if placeholder not in text:
            continue
        # Coerce non-str values via the shared repository structured-data
        # convention (JSON for dict/list, str() for scalars). This defends
        # against ctx.state values that are not strings (e.g., dicts stashed
        # by upstream nodes) and, if serialization itself ever fails, falls
        # back to a key+type-only message rather than crashing or leaking
        # the value's content (jleechan-7t92).
        text = text.replace(placeholder, _serialize_state_value(k, v))
    # The slim worker prompt uses this ordinary state placeholder to receive
    # reviewer findings on retries. Its deterministic first-visit default
    # prevents the start-node output from being misrepresented as feedback.
    text = text.replace(
        "${state._last_review_feedback}",
        "(no prior reviewer feedback)",
    )
    diff = ctx.state.get("_last_diff", "")
    if not diff:
        diff = "(no diff captured)"
    text = text.replace("${diff}", diff)

    changed_files = ctx.state.get("_last_changed_files", "")
    if not changed_files:
        changed_files = "(none)"
    text = text.replace("${changed_files}", changed_files)

    if "${lint_findings}" in text:
        findings = _resolve_lint_findings(ctx)
        text = text.replace("${lint_findings}", findings_to_markdown(findings))
    return text


def _render_prompt(node: "Node", ctx: "Context") -> str:
    import runner.handlers as _handlers_shim  # late-bound shim (see module docstring)
    backend = node.attrs.get("backend", node.attrs.get("model", ctx.backend))
    if isinstance(backend, bool):
        backend = ctx.backend
    backend = str(backend)

    orig_last_output = ctx.state.get("_last_output")
    if backend == "agy" and orig_last_output is not None:
        ctx.state["_last_output"] = orig_last_output[:4000]

    try:
        if _is_fresh_review_node(node, backend):
            cached = _cached_fresh_review_prompt(node, backend, ctx)
            if cached is not None:
                return str(cached["rendered"])
            pinned_source = _fresh_review_prompt_source(node, backend)
            if pinned_source is None:
                rendered = (
                    f"# {node.name}\n\n"
                    "Factory-owned fresh-review authority is unavailable."
                )
                setattr(
                    ctx,
                    "_fresh_review_prompt_cache",
                    {
                        "key": _fresh_review_prompt_cache_key(node, backend, ctx),
                        "source_path": "",
                        "source_text": "",
                        "source_bytes": b"",
                        "rendered": rendered,
                    },
                )
                return rendered
            source_path, source_bytes = pinned_source
            try:
                source_text = source_bytes.decode("utf-8")
            except UnicodeDecodeError:
                rendered = (
                    f"# {node.name}\n\n"
                    "Factory-owned fresh-review authority is unavailable."
                )
                source_path = ""
                source_text = ""
                source_bytes = b""
                setattr(
                    ctx,
                    "_fresh_review_prompt_cache",
                    {
                        "key": _fresh_review_prompt_cache_key(node, backend, ctx),
                        "source_path": source_path,
                        "source_text": source_text,
                        "source_bytes": source_bytes,
                        "rendered": rendered,
                    },
                )
                return rendered
            rendered = _substitute_placeholders(source_text, ctx)
            setattr(
                ctx,
                "_fresh_review_prompt_cache",
                {
                    "key": _fresh_review_prompt_cache_key(node, backend, ctx),
                    "source_path": str(source_path),
                    "source_text": source_text,
                    "source_bytes": source_bytes,
                    "rendered": rendered,
                },
            )
            return rendered
        ref = node.prompt_ref
        if not ref:
            return f"# {node.name}\n\nGoal: {ctx.goal}"
        ref_path = pathlib.Path(ref)
        if ref_path.is_absolute():
            resolved_ref = ref_path
            try:
                resolved_ref = ref_path.resolve()
            except FileNotFoundError:
                return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"

            for deny in _handlers_shim._holdout_denied_paths():
                try:
                    resolved_ref.relative_to(deny)
                except ValueError:
                    pass
                else:
                    return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"

            text_path = resolved_ref
            if not text_path.exists():
                return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"
            text = text_path.read_text()
            return _substitute_placeholders(text, ctx)
        from .paths import factory_home
        root = ctx.workdir.resolve()
        p = (root / ref_path).resolve()
        if not p.exists():
            home = factory_home()
            if home is not None:
                alt = (home / ref_path).resolve()
                if alt.exists():
                    p = alt
        try:
            p.relative_to(root)
        except ValueError:
            home = factory_home()
            if home is not None:
                try:
                    p.relative_to(home.resolve())
                except ValueError:
                    return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"
            else:
                return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"
        if not p.exists():
            return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"
        text = p.read_text()
        return _substitute_placeholders(text, ctx)
    finally:
        if orig_last_output is not None:
            ctx.state["_last_output"] = orig_last_output
