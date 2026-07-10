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
"""

from __future__ import annotations

import json
import pathlib
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim
from .pre_review_lint import findings_to_markdown, findings_to_json, lint_findings

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _resolve_lint_findings(ctx: "Context") -> list[dict]:
    """Return the cached lint findings, computing on first call."""
    import json
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
        # Coerce non-str values to string for substitution. This defends against
        # ctx.state values that are not strings (e.g., dicts stored by
        # _resolve_gate_backend in handler_dispatch.py).
        if isinstance(v, str):
            rendered = v
        elif isinstance(v, (dict, list)):
            rendered = json.dumps(v, sort_keys=True)
        else:
            rendered = str(v)
        text = text.replace(placeholder, rendered)
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
    backend = node.attrs.get("backend", node.attrs.get("model", ctx.backend))
    if isinstance(backend, bool):
        backend = ctx.backend
    backend = str(backend)

    orig_last_output = ctx.state.get("_last_output")
    if backend == "agy" and orig_last_output is not None:
        ctx.state["_last_output"] = orig_last_output[:4000]

    try:
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
