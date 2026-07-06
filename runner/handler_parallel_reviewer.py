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

import runner.handlers as _handlers_shim

from .handler_core import Result
from .handler_core import _gate_strict_flag
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


def _parallel_reviewer(node: "Node", ctx: "Context") -> "Result":
    """Run parallel reviewer lanes and pass combined evidence downstream."""
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
    return Result(
        outcome=final_outcome,
        output=result.output,
        metadata=merged_metadata,
        preferred_label=result.preferred_label,
        suggested_next_ids=result.suggested_next_ids,
        context_updates=result.context_updates,
    )
