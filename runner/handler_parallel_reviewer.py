"""Parallel reviewer handler.

Runs a primary reviewer lane and a shadow Codex reviewer lane in parallel,
then merges both outputs for one downstream handoff.

Implementation note: this reuses existing gate-review helpers so runtime
behavior is aligned with existing lanes (`_resolve_gate_backend`,
`_start_shadow_gate_review`, `_finish_shadow_gate_review`).
"""

from __future__ import annotations

import json
from typing import TYPE_CHECKING

# Import collaborators from their source modules directly. Importing
# `runner.handlers` (the re-export shim) here would create a module-load
# cycle: handlers.py imports this file at TYPE_REGISTRY registration time,
# and at module-load that partial re-import would re-enter handlers before
# `_parallel_reviewer` is bound. The shim still re-exports these names, so
# downstream monkeypatching via `runner.handlers._render_prompt` etc. keeps
# working unchanged.
#
# Symbols that tests monkeypatch via ``runner.handlers._X`` are looked up
# lazily inside ``_parallel_reviewer`` via the shim — see the function body.
from .handler_core import Result
from .handler_core import _gate_strict_flag
# Canonical implementation lives in handler_verdict (pr228 B1 relocation).
# Re-exported here for backward compatibility: handler_verdict is a leaf
# module (imports nothing from handlers), so this creates no import cycle.
from .handler_verdict import _enforce_outcome_verdict_consistency  # noqa: F401
from .handler_dispatch import (
    _finish_shadow_gate_review,
    _is_gate_infra_failure,
    _resolve_gate_backend,
    _execute_gate,
    _launch_shadow_gate_review,
    _parse_priority_env,
    _start_shadow_gate_review,
)

if TYPE_CHECKING:
    from .handler_core import Context
    from .parser import Node


def _shadow_codex_review_enabled(ctx: "Context") -> bool:
    """Return true when shadow Codex should run for this context."""
    raw = ctx.state.get("_df_shadow_codex_review", "false")
    if isinstance(raw, str):
        return raw.strip().lower() in {"true", "1", "yes", "on"}
    return bool(raw)


def _parse_shadow_backends(ctx: "Context") -> list[str]:
    """Return the list of shadow reviewer backends for this context.

    Reads ``ctx.state["_df_shadow_backends"]`` (comma-separated, same parser
    as the adversarial priority queue) and returns the cleaned list. Empty or
    missing input yields ``[]``, which signals the orchestrator to fall back
    to the legacy single-codex shadow gate.
    """
    raw = ctx.state.get("_df_shadow_backends", "")
    if not isinstance(raw, str):
        raw = str(raw)
    return _parse_priority_env(raw)


def _run_primary_review(
    prompt: str,
    expected_sha: str,
    timeout: int,
    ctx: "Context",
    node_name: str,
    backend: str,
    *,
    gate_strict: bool,
) -> Result:
    """Run the primary reviewer lane with infra fallback policy.

    This keeps behavior consistent with gate reviewer fallback: infra failures
    are retried once on claude, never reviewer-shopping on real pass/fail.
    """
    prior_shadow_flag = ctx.state.get("_df_shadow_codex_review")
    ctx.state["_df_shadow_codex_review"] = "false"
    try:
        result = _execute_gate(
            prompt,
            expected_sha,
            timeout,
            ctx,
            node_name,
            backend,
            gate_strict=gate_strict,
        )
    finally:
        if prior_shadow_flag is None:
            ctx.state.pop("_df_shadow_codex_review", None)
        else:
            ctx.state["_df_shadow_codex_review"] = prior_shadow_flag

    return result


def _record_primary_output(
    node_name: str,
    attempt: int,
    result: "Result",
    seq: int,
    ctx: "Context",
) -> Result:
    """Persist primary output/output hash and emit a dedicated event trail."""
    output_path = ""
    output_sha = ""
    try:
        from . import engine_observability as _obs

        output_path, output_sha = _obs._write_input_sidecar(
            ctx,
            seq,
            node_name,
            attempt,
            result.output,
            kind="parallel_reviewer_primary_output",
        )
        _obs._emit_event(
            ctx,
            "parallel_reviewer_primary_result",
            {
                "node": node_name,
                "attempt": str(attempt),
                "outcome": result.outcome,
                "primary_review_output_path": output_path or "",
                "primary_review_output_sha256": output_sha or "",
            },
            seq,
        )
    except Exception:
        pass

    md = dict(result.metadata)
    md.update(
        {
            "parallel_reviewer_primary_outcome": result.outcome,
            "parallel_reviewer_primary_output_path": output_path or "",
            "parallel_reviewer_primary_output_sha256": output_sha or "",
            "parallel_reviewer_primary_prompt_path": md.get("llm_prompt_path", ""),
            "parallel_reviewer_primary_prompt_sha256": md.get("llm_prompt_sha256", ""),
        }
    )
    updates = dict(result.context_updates)
    updates[f"{node_name}.primary_review_outcome"] = result.outcome
    updates[f"{node_name}.primary_review_output_path"] = output_path or ""
    return Result(
        outcome=result.outcome,
        output=result.output,
        metadata=md,
        preferred_label=result.preferred_label,
        suggested_next_ids=result.suggested_next_ids,
        context_updates=updates,
    )


def _coalesce_parallel_outcome(primary: str, shadows: list[str]) -> str:
    """Conservative N-way merge: all lanes must pass for success; any errored lane => error; else failure."""
    outcomes = [primary, *shadows]
    if any(o == "error" for o in outcomes):
        return "error"
    if all(o == "success" for o in outcomes):
        return "success"
    return "failure"


def _receipt_required_flag(node: "Node") -> bool:
    """Read the ``receipt_required`` node attribute as a bool.

    Same acceptance rules as ``_gate_strict_flag``: ``True`` / ``"true"`` /
    ``"1"`` / ``"yes"`` (case-insensitive); anything else is False so
    existing graphs do not regress. When True, a reviewer success is only
    kept if the review transcript carries a reproduction receipt — a real
    build/test runner AND a captured exit code 0 (see
    ``handler_verdict._reproduction_receipt_gap``).
    """
    raw = node.attrs.get("receipt_required")
    if raw is True:
        return True
    return isinstance(raw, str) and raw.strip().lower() in ("true", "1", "yes")


def _enforce_reproduction_receipt(result: "Result") -> "Result":
    """Downgrade a reviewer success whose transcript lacks a reproduction receipt.

    A reviewer that verdicts PASS without re-running the build/test (or whose
    re-run FAILED, nonzero-only exit trail) is read-only theater — the exact
    self-reported-verdict hole the receipt gate closes. Only success outcomes
    are touched; failure/error pass through so route-back reasons are never
    masked. Mirrors the ``verdict_adjusted_for_consistency`` audit pattern:
    the original verdict is preserved in metadata.
    """
    # Lazy import to avoid circular import at module load time
    from .handler_verdict import _reproduction_receipt_gap

    if result.outcome != "success":
        return result
    gap = _reproduction_receipt_gap(result.output or "")
    if not gap:
        return result
    new_md = dict(result.metadata or {})
    new_md["original_verdict"] = str(new_md.get("verdict", ""))
    new_md["verdict"] = "fail"
    new_md["receipt_downgraded"] = "true"
    new_md["receipt_gap"] = gap
    return Result(
        outcome="failure",
        output=(result.output or "") + "\n\n" + gap,
        metadata=new_md,
        preferred_label=result.preferred_label,
        suggested_next_ids=result.suggested_next_ids,
        context_updates=result.context_updates,
    )


def _parallel_reviewer(node: "Node", ctx: "Context") -> "Result":
    """Run parallel reviewer lanes and pass combined evidence downstream."""
    import runner.handlers as _handlers_shim  # late-bound shim for monkeypatched helpers
    prompt = _handlers_shim._render_prompt(node, ctx)
    if ctx.backend in ("echo", "mock_llm"):
        hint = ctx.state.get(f"{node.name}.outcome", "success")
        return Result(
            outcome=hint,
            output=f"echo parallel reviewer {node.name}: pre-seeded {hint}",
            metadata={
                "slash_command": node.name,
                "verdict": "echo:" + str(hint),
                "reviewer_backend": str(ctx.backend),
                "parallel_reviewer": "echo",
            },
        )
    backend, backend_meta = _resolve_gate_backend(node, ctx)

    timeout = _handlers_shim._coerce_timeout(
        node.attrs.get("timeout", "1200"),
        1200,
    )
    expected_sha = _handlers_shim._worktree_head_sha(ctx.workdir)
    gate_strict = _gate_strict_flag(node)

    # Determine shadow lanes BEFORE running primary so Popen launches happen
    # before primary (and before any communicate()). True concurrency.
    shadow_backends = _parse_shadow_backends(ctx)
    shadows = []
    if shadow_backends:
        for b in shadow_backends:
            shadows.append(_launch_shadow_gate_review(
                node.name, prompt, expected_sha, timeout, ctx, backend=b,
            ))
    elif _shadow_codex_review_enabled(ctx):
        s = _start_shadow_gate_review(node.name, prompt, expected_sha, timeout, ctx)
        if s:
            shadows.append(s)

    primary = _run_primary_review(
        prompt,
        expected_sha,
        timeout,
        ctx,
        node.name,
        backend,
        gate_strict=gate_strict,
    )

    seq = int(getattr(ctx, "_df_current_seq", getattr(ctx, "last_completed_seq", 0)))
    attempt = int(getattr(ctx, "_df_current_attempt", 1))
    primary = _record_primary_output(node.name, attempt, primary, seq, ctx)
    primary.metadata.update(backend_meta)

    if not shadows:
        # Bug 2 fix: Ensure outcome and verdict are consistent. A contradictory
        # verdict (e.g., outcome=failure with verdict=pass) can occur when stale
        # spec artifacts cause the reviewer to misjudge. Force verdict to match outcome.
        # Dispatch via the canonical re-export shim so the unqualified name here
        # reaches the single definition in ``runner.handler_verdict``. Then,
        # when the graph declares ``receipt_required="true"`` on this
        # parallel-reviewer node, downgrade a success whose transcript lacks
        # a reproduction receipt (real build/test runner + exit code 0).
        primary = _handlers_shim._enforce_outcome_verdict_consistency(
            primary, gate_strict=gate_strict,
        )
        if _receipt_required_flag(node):
            primary = _enforce_reproduction_receipt(primary)
        return primary

    result = primary
    shadow_outcomes = []
    for shadow in shadows:
        result = _finish_shadow_gate_review(result, shadow, node.name, expected_sha, timeout, ctx)
        shadow_outcomes.append(str(result.metadata.get(f"shadow_{shadow.backend}_gate_outcome", "unknown")))
    final_outcome = _coalesce_parallel_outcome(primary.outcome, shadow_outcomes)
    shadow_reviews = {
        s.backend: {
            "outcome": str(result.metadata.get(f"shadow_{s.backend}_gate_outcome", "unknown")),
            "verdict": str(result.metadata.get(f"shadow_{s.backend}_gate_verdict", "unknown")),
            "head_sha_status": str(result.metadata.get(f"shadow_{s.backend}_gate_head_sha_status", "unknown")),
        }
        for s in shadows
    }
    merged_metadata = dict(result.metadata)
    merged_metadata["parallel_reviewer_shadow_backends"] = ",".join(s.backend for s in shadows)
    merged_metadata["shadow_reviews"] = json.dumps(shadow_reviews, sort_keys=True)
    merged_metadata["parallel_reviewer_outcome"] = final_outcome
    # Build result and enforce outcome/verdict consistency.
    final_result = Result(
        outcome=final_outcome,
        output=result.output,
        metadata=merged_metadata,
        preferred_label=result.preferred_label,
        suggested_next_ids=result.suggested_next_ids,
        context_updates=result.context_updates,
    )
    final_result = _handlers_shim._enforce_outcome_verdict_consistency(
        final_result, gate_strict=gate_strict,
    )
    if _receipt_required_flag(node):
        final_result = _enforce_reproduction_receipt(final_result)
    return final_result
