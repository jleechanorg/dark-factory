"""Parallel reviewer handler.

Runs a primary reviewer lane and (optionally) one or more shadow Codex
reviewer lanes in parallel, then merges all outputs into one downstream
handoff.

Single-shadow behavior is the legacy default, preserved verbatim for back-
compat. The N-shadow fan-out (``n_shadows="N"`` node attribute) is the
2026-07 qw5-pilot extension — see bead jleechan-qw5 and the binding spec
at daemon/qw5-coder-prompt.md.

Coalesce semantics (binding per /advice 2026-06-27 Reviewer A):
  * ANY shadow ``error``     → overall ``error``   (infra failure surfaces)
  * ALL shadows + primary    → ``success``          (no reviewer-shopping)
  * otherwise                → ``failure``

Implementation note: this reuses existing gate-review helpers so runtime
behavior is aligned with existing lanes (``_resolve_gate_backend``,
``_start_shadow_gate_review``, ``_finish_shadow_gate_review``).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

from .handler_core import Result
from .handler_core import _gate_strict_flag
from .handler_dispatch import (
    _coalesce_n_shadow_outcomes,
    _finish_shadow_gate_review,
    _is_gate_infra_failure,
    _resolve_gate_backend,
    _execute_gate,
    _start_shadow_gate_review,
)

if TYPE_CHECKING:
    from .handler_core import Context
    from .parser import Node


def _shadow_codex_review_enabled(ctx: "Context") -> bool:
    """Return true when the legacy shadow Codex state flag enables 1 shadow."""
    raw = ctx.state.get("_df_shadow_codex_review", "false")
    if isinstance(raw, str):
        return raw.strip().lower() in {"true", "1", "yes", "on"}
    return bool(raw)


def _resolve_n_shadows(node: "Node", ctx: "Context") -> int:
    """Resolve how many Codex shadows to fan out for this node.

    Order:
      1. ``n_shadows`` node attribute (positive int) wins.
      2. Legacy ``ctx.state["_df_shadow_codex_review"]`` flag enables 1 shadow.
      3. Otherwise 0.
    """
    raw = node.attrs.get("n_shadows")
    if raw is not None and str(raw).strip() != "":
        try:
            n = int(str(raw).strip())
        except (TypeError, ValueError):
            n = -1
        if n < 0:
            return 0
        return max(0, n)
    if _shadow_codex_review_enabled(ctx):
        return 1
    return 0


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
        # minimax shares the claude CLI binary but grades via a different
        # gateway/model, so claude remains a meaningful infra fallback.
        claude_routed = backend in ("claude", "claude-sonnet")
        if _is_gate_infra_failure(result) and not claude_routed:
            fallback = _execute_gate(
                prompt,
                expected_sha,
                timeout,
                ctx,
                node_name,
                "claude",
                gate_strict=gate_strict,
            )
            fallback.metadata["fallback_used"] = "true"
            fallback.metadata["fallback_from"] = backend
            if _is_gate_infra_failure(fallback):
                fallback.metadata["verdict"] = "infra_failure"
            result = fallback
    finally:
        if prior_shadow_flag is None:
            ctx.state.pop("_df_shadow_codex_review", None)
        else:
            ctx.state["_df_shadow_codex_review"] = prior_shadow_flag

    result.metadata.setdefault("fallback_used", "false")
    if _is_gate_infra_failure(result):
        result.metadata["verdict"] = "infra_failure"
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


def _coalesce_parallel_outcome(primary: str, shadow: str) -> str:
    """Conservative merge: both lanes must pass for success."""
    if primary == "success" and shadow == "success":
        return "success"
    if primary == "error" or shadow == "error":
        return "error"
    return "failure"


def _parallel_reviewer(node: "Node", ctx: "Context") -> "Result":
    """Run parallel reviewer lanes and pass combined evidence downstream.

    Resolution order for the shadow count:
      1. ``n_shadows`` node attribute (1 → legacy 1-shadow; N>=2 → fan-out).
      2. Legacy ctx.state["_df_shadow_codex_review"] → 1 shadow.
      3. Otherwise no shadow — only the primary reviewer runs.
    """
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

    n_shadows = _resolve_n_shadows(node, ctx)

    # ─── 0-shadow path: no fan-out, primary only. ────────────────────────
    if n_shadows == 0:
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
        return primary

    # ─── N-shadow fan-out path (n_shadows >= 1). ─────────────────────────
    # Spawn ALL shadows before joining ANY (subprocess.Popen launches run
    # truly concurrently). Then sequential communicate() just waits on the
    # already-running processes; wall-clock = max(shadow runtimes), not sum.
    shadows = []
    # The n_shadows node attribute is itself an opt-in to bypass the legacy
    # `_df_shadow_codex_review` gate inside `_start_shadow_gate_review`.
    # Restore the prior state after the spawn so downstream handlers see the
    # user's original signal.
    prior_shadow_flag = ctx.state.get("_df_shadow_codex_review")
    ctx.state["_df_shadow_codex_review"] = "true"
    try:
        if n_shadows >= 2:
            # True N-shadow fan-out: each slot gets a 1-based shadow_index so
            # its metadata keys + events are distinguishable downstream.
            for i in range(1, n_shadows + 1):
                shadows.append(
                    _start_shadow_gate_review(
                        node.name,
                        prompt,
                        expected_sha,
                        timeout,
                        ctx,
                        shadow_index=i,
                    )
                )
        else:
            # n_shadows == 1 — always legacy single-shadow keys (back-compat).
            shadows.append(
                _start_shadow_gate_review(
                    node.name,
                    prompt,
                    expected_sha,
                    timeout,
                    ctx,
                    shadow_index=None,
                )
            )
    finally:
        if prior_shadow_flag is None:
            ctx.state.pop("_df_shadow_codex_review", None)
        else:
            ctx.state["_df_shadow_codex_review"] = prior_shadow_flag

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

    # Single-shadow legacy behavior: idx=None keeps back-compat metadata.
    if n_shadows == 1:
        shadow = shadows[0]
        if shadow is None:
            return primary
        legacy = _finish_shadow_gate_review(
            primary, shadow, node.name, expected_sha, timeout, ctx
        )
        shadow_outcome = str(legacy.metadata.get("shadow_codex_gate_outcome", "unknown"))
        final_outcome = _coalesce_parallel_outcome(primary.outcome, shadow_outcome)
        if final_outcome != legacy.outcome:
            merged_metadata = dict(legacy.metadata)
            merged_metadata["parallel_reviewer_outcome"] = final_outcome
            return Result(
                outcome=final_outcome,
                output=legacy.output,
                metadata=merged_metadata,
                preferred_label=legacy.preferred_label,
                suggested_next_ids=legacy.suggested_next_ids,
                context_updates=legacy.context_updates,
            )
        return legacy

    # N >= 2: per-slot finish + conservative coalesce.
    shadow_outcomes: list[tuple[int | None, str, str]] = []
    accumulated = primary
    for i, shadow in enumerate(shadows, start=1):
        accumulated = _finish_shadow_gate_review(
            accumulated, shadow, node.name, expected_sha, timeout, ctx,
            shadow_index=i,
        )
        out = str(accumulated.metadata.get(f"shadow_codex_gate_outcome_{i}", "unknown"))
        raw_verdict = str(accumulated.metadata.get(f"shadow_codex_gate_verdict_{i}", "unknown"))
        shadow_outcomes.append((i, out, raw_verdict))

    final_outcome, parallel_label = _coalesce_n_shadow_outcomes(
        primary.outcome, shadow_outcomes
    )
    merged_metadata = dict(accumulated.metadata)
    merged_metadata["parallel_reviewer_outcome"] = parallel_label
    merged_metadata["parallel_reviewer_n_shadows"] = str(n_shadows)
    merged_metadata["parallel_reviewer_shadow_outcomes"] = ",".join(
        f"{idx or 0}:{o}:{r}" for idx, o, r in shadow_outcomes
    )

    return Result(
        outcome=final_outcome,
        output=accumulated.output,
        metadata=merged_metadata,
        preferred_label=accumulated.preferred_label,
        suggested_next_ids=accumulated.suggested_next_ids,
        context_updates=accumulated.context_updates,
    )
