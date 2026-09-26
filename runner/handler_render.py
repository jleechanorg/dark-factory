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
  * ``${target}``   → the runner-minted ``factory.review-target.v1`` locator
    canonical string (``ctx.state["target"]``, D3). Defaults to
    ``"(no target minted)"`` when unset.
  * ``${intent}``   → the runner-minted, Base64-encoded task-record envelope
    (``ctx.state["intent"]``, D2). Defaults to a Base64-encoded
    "(none — target-mode verification run)" placeholder when unset, so the
    fence's "Base64-encoded" claim always holds.

Reviewer-class fail-closed rendering (D1/D2, two-node redesign v3):
  * For the fresh, verdict-gated cold-reviewer contract (``class="review"``
    AND ``verdict_gate="true"`` — see ``_is_review_node``; this deliberately
    excludes older, unrelated ``class="review"`` gate/shadow-review nodes
    that still read ``${goal}`` under their own contract), every
    ``_render_prompt`` fallback path (missing template ref, unresolved/
    escaped/denied path, missing file) raises ``ReviewPromptRenderError``
    instead of returning a ``Goal: {ctx.goal}`` stub — a reviewer never runs
    on a degraded prompt.
  * After substitution, a review-class render additionally asserts the
    rendered text contains no literal ``ctx.goal`` outside the fenced TASK
    RECORD block, and no unsubstituted ``${target}``/``${intent}`` literal.
    Either failure raises ``ReviewPromptRenderError`` (fail closed).

The ``runner.handlers._holdout_denied_paths`` symbol is looked up via the
shim at runtime (lazy import inside ``_render_prompt``) so existing test
monkeypatching via ``monkeypatch.setattr("runner.handlers._holdout_denied_paths", ...)``
keeps working. The shim is NOT imported at module top to avoid a load-time
cycle: ``runner.handlers`` imports this file, and a partial re-import would
re-enter the shim before ``_holdout_denied_paths`` was bound.
"""

from __future__ import annotations

import json
import pathlib
from typing import TYPE_CHECKING

from .handler_core import _serialize_state_value
from .pre_review_lint import findings_to_markdown, findings_to_json, lint_findings

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context

import base64

_TASK_RECORD_BEGIN = "BEGIN TASK RECORD"
_TASK_RECORD_END = "END TASK RECORD"

_DEFAULT_INTENT_TEXT = "(none — target-mode verification run)"


class ReviewPromptRenderError(Exception):
    """Raised when a ``class="review"`` node's prompt cannot render safely.

    Covers both the fallback-abolition case (missing/escaped/denied
    template) and the render-time goal-leak / unsubstituted-placeholder
    assertion. Callers (``_codergen``) must abort the visit as ``failure``
    (fail closed) — a reviewer never runs on a degraded or leaking prompt.
    """


def _is_review_node(node: "Node") -> bool:
    """True for the fresh, verdict-gated cold reviewer contract (D1/D2)
    only — ``class="review"`` alone also covers unrelated, pre-existing
    graph-authored review/gate nodes elsewhere in the factory (gate_es,
    gate_er, shadow-review comparisons, ...) that legitimately read
    ``${goal}`` under their own, older contract and are out of scope for
    the two-node redesign's fail-closed rendering rules.
    """
    is_review_class = str(node.attrs.get("class", "")).strip().lower() == "review"
    is_verdict_gated = str(node.attrs.get("verdict_gate", "false")).strip().lower() in {
        "true", "1", "yes", "on",
    }
    return is_review_class and is_verdict_gated


def _strip_fenced_section(text: str) -> str:
    """Remove the TASK RECORD fence (if present) so the goal-leak check only
    inspects text outside the runner-minted, Base64-encoded envelope."""
    begin_idx = text.find(_TASK_RECORD_BEGIN)
    end_idx = text.find(_TASK_RECORD_END)
    if begin_idx == -1 or end_idx == -1 or end_idx < begin_idx:
        return text
    end_idx += len(_TASK_RECORD_END)
    return text[:begin_idx] + text[end_idx:]


def _assert_reviewer_render_safe(rendered: str, ctx: "Context") -> None:
    """D1 render-time assertion: no caller/worker text reaches the reviewer
    outside the fenced envelope, and every first-class placeholder resolved."""
    if "${target}" in rendered or "${intent}" in rendered:
        raise ReviewPromptRenderError(
            "reviewer prompt contains an unsubstituted ${target}/${intent} literal"
        )
    goal = (getattr(ctx, "goal", "") or "").strip()
    if goal and goal in _strip_fenced_section(rendered):
        raise ReviewPromptRenderError(
            "reviewer prompt leaks ctx.goal text outside the TASK RECORD fence"
        )
    # D3/D8a fail-closed (external-review finding): a mint failure after a
    # successful worker visit must never let the reviewer silently run
    # against the "${target}" placeholder default — `_mint_post_worker_target`
    # is best-effort by design, so this is the last line of defense before a
    # reviewer would launch with no real pin at all.
    if "(no target minted)" in rendered:
        raise ReviewPromptRenderError(
            "reviewer prompt contains the unminted-target placeholder "
            '"(no target minted)" — the review target was never minted '
            "(fail closed)"
        )


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

    target = ctx.state.get("target") or "(no target minted)"
    text = text.replace("${target}", str(target))

    intent = ctx.state.get("intent")
    if not intent:
        intent = base64.b64encode(_DEFAULT_INTENT_TEXT.encode("utf-8")).decode("ascii")
    text = text.replace("${intent}", str(intent))
    return text


def _render_prompt(node: "Node", ctx: "Context") -> str:
    import runner.handlers as _handlers_shim  # late-bound shim (see module docstring)
    backend = node.attrs.get("backend", node.attrs.get("model", ctx.backend))
    if isinstance(backend, bool):
        backend = ctx.backend
    backend = str(backend)
    is_review = _is_review_node(node)

    def _fallback(reason: str, ref: str = "") -> str:
        # Reviewer-class fallback stubs are abolished (v3 delta): a
        # class="review" node never runs on a degraded Goal-stub prompt.
        if is_review:
            raise ReviewPromptRenderError(
                f"reviewer prompt render fallback triggered ({reason}): {ref}"
            )
        suffix = f"\n({reason}: {ref})" if reason and ref else ""
        return f"# {node.name}\n\nGoal: {ctx.goal}{suffix}"

    def _rendered(text: str) -> str:
        rendered = _substitute_placeholders(text, ctx)
        if is_review:
            _assert_reviewer_render_safe(rendered, ctx)
        return rendered

    orig_last_output = ctx.state.get("_last_output")
    if backend == "agy" and orig_last_output is not None:
        ctx.state["_last_output"] = orig_last_output[:4000]

    try:
        ref = node.prompt_ref
        if not ref:
            return _fallback("no prompt ref")
        ref_path = pathlib.Path(ref)
        if is_review and ref_path.is_absolute():
            # Round-8 adversarial finding: the round-7 fix routed an
            # absolute path through trusted-root containment, which still
            # ACCEPTED it when the path happened to resolve inside the
            # trusted root — contradicting this fix's own "refuse
            # absolute... paths" framing and the repo-wide contract
            # (CLAUDE.md) that prompt refs are always
            # ``prompt="@relative/path.md"``. No legitimate `.dot` graph
            # ever writes an absolute prompt path; refuse it outright, with
            # no trusted-root exception.
            return _fallback("absolute prompt path refused", ref)
        if ref_path.is_absolute():
            resolved_ref = ref_path
            try:
                resolved_ref = ref_path.resolve()
            except FileNotFoundError:
                return _fallback("missing prompt", ref)

            for deny in _handlers_shim._holdout_denied_paths():
                try:
                    resolved_ref.relative_to(deny)
                except ValueError:
                    pass
                else:
                    return _fallback("invalid prompt", ref)

            text_path = resolved_ref
            if not text_path.exists():
                return _fallback("missing prompt", ref)
            text = text_path.read_text()
            return _rendered(text)
        from .paths import factory_home
        if is_review:
            # CRITICAL-2 (external review, round 3): a reviewer-class node's
            # prompt template must come from the factory's own trusted
            # installation, never from `ctx.workdir` — the live coder
            # worktree the implementing agent just edited. A worker could
            # otherwise overwrite `prompts/slim/fresh_review.md` in its own
            # workdir (a real risk: dark-factory's own coder workdir is
            # frequently a checkout of this same repo) and replace the
            # reviewer's governing instructions before the reviewer runs.
            # `$DARK_FACTORY_HOME` wins when set; otherwise fall back to the
            # runner package's own repo root (derived from `__file__`, which
            # is never influenced by the coder's workdir).
            trusted_root = factory_home()
            if trusted_root is None:
                trusted_root = pathlib.Path(__file__).resolve().parent.parent
            p = (trusted_root / ref_path).resolve()
            try:
                p.relative_to(trusted_root.resolve())
            except ValueError:
                return _fallback("invalid prompt", ref)
            for deny in _handlers_shim._holdout_denied_paths():
                try:
                    p.relative_to(deny)
                except ValueError:
                    pass
                else:
                    return _fallback("invalid prompt", ref)
            if not p.exists():
                return _fallback("missing prompt", ref)
            text = p.read_text()
            return _rendered(text)
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
                    return _fallback("invalid prompt", ref)
            else:
                return _fallback("invalid prompt", ref)
        if not p.exists():
            return _fallback("missing prompt", ref)
        text = p.read_text()
        return _rendered(text)
    finally:
        if orig_last_output is not None:
            ctx.state["_last_output"] = orig_last_output
