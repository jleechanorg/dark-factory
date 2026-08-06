"""Shared helper for the parallel Codex shadow-review lane.

Extracted from ``runner.handler_codergen`` so any caller (the codergen
reviewer pathway today, plus any future handler that wants the same
parity) can drive a one-shot ``codex exec --yolo`` review with the same:

  * workdir sandbox profile (``_sandboxed_args_for_workdir`` — see
    jleechan-113) that denies ``$DARK_FACTORY_HOLDOUTS`` and any
    ``<workdir>/benchmarks/*/`` sealed-doc files;
  * ``os.killpg`` SIGTERM-then-SIGKILL escalation on timeout, so
    subprocesses leave no orphan grandchildren behind (the dispatch
    gate shadow path uses ``proc.kill()`` only — this codergen
    helper preserves the stricter killpg cascade);
  * optional ``expected_sha`` SHA-echo verification: the codergen
    path passes ``None`` (no parity check; the implementing agent
    writes inside its AO-managed worktree so there is no canonical
    head_sha to bind to a frozen reviewer). The dispatch gate
    pathway, when it adopts this helper, will pass a real SHA so
    the helper can enforce ``_verify_head_sha_echo`` and downgrade
    ``success``/``unknown`` outcomes to ``error`` on mismatch.
  * hardcoded ``shadow_codex_*`` metadata keys (not the parametric
    ``shadow_{backend}_gate_*`` used by the dispatch priority-queue
    shadow lane) and the literal ``## Parallel Codex Review`` block
    that ``tests/test_codergen_shadow_review.py`` and
    ``tests/test_state_threading.py`` hardcode — those tests must
    remain unchanged after the dedup.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import signal
import subprocess
import time
from dataclasses import dataclass

import runner.handlers as _handlers_shim

# Event-log sidecar + observability emit match the original
# ``handler_codergen._start_shadow_codex_review`` shape exactly so
# downstream CXDB consumers see no protocol change.


@dataclass
class _ShadowCodexReview:
    prompt: str = ""
    proc: subprocess.Popen | None = None
    prompt_path: str = ""
    prompt_sha256: str = ""
    launch_error: str = ""
    started_at: float = 0.0


def _enabled(node, ctx) -> bool:
    """Codergen shadow-review enable predicate (unchanged from handler_codergen).

    Honors the ``shadow_codex_review`` node attr and the
    ``_df_shadow_codex_review`` context-state override.
    """
    raw = node.attrs.get("shadow_codex_review", ctx.state.get("_df_shadow_codex_review", "false"))
    if isinstance(raw, str) and raw.strip().lower() in {"false", "0", "no", "off"}:
        return False
    if raw is False:
        return False
    return str(node.attrs.get("class", "")).strip().lower() == "review"


def _build_prompt(node, ctx, primary_prompt: str) -> str:
    """Build the simple independent reviewer prompt (unchanged from handler_codergen)."""
    review_target = str(node.attrs.get("shadow_review_target", "diff")).strip() or "diff"
    diff = ctx.state.get("_last_diff", "(no diff captured)")
    changed_files = ctx.state.get("_last_changed_files", "(no changed files captured)")
    previous = ctx.state.get("_last_output", "")
    return f"""\
review this {review_target}

You are the parallel Codex reviewer for a Dark Factory reviewer node.
Do an independent, blocker-first review of the current workspace. Focus on
what a coder can fix next, not on restating gate status.

Goal:
{ctx.goal}

Reviewer node:
{node.name}

Changed files:
{changed_files}

Diff captured before this reviewer:
```
{diff}
```

Previous node output:
```
{previous}
```

Primary reviewer prompt for comparison:
```
{primary_prompt}
```

Return this exact free-form shape:

## Review Verdict
pass | fail

## Blocking Findings
1. Severity: concise issue title.
   Evidence: exact file/function/run/artifact/line or say none.
   Why it matters: behavioral or merge-readiness impact.
   Fix: smallest concrete coder action.

## Evidence Checked
- Exact commands, files, logs, screenshots, videos, URLs, or artifacts inspected.

## Required Next Actions
1. Smallest patch or evidence regeneration step.
2. Exact verification command or artifact to rerun.

End with this machine-readable routing line:
verdict: <pass|fail>
"""


def _emit_prompt_sidecar(shadow: _ShadowCodexReview, node, ctx, prompt: str) -> None:
    """Record the shadow prompt to CXDB; failures swallowed (unchanged behavior)."""
    try:
        from . import engine_observability as _obs

        seq = int(getattr(ctx, "_df_current_seq", getattr(ctx, "last_completed_seq", 0)))
        attempt = int(getattr(ctx, "_df_current_attempt", 1))
        prompt_path, prompt_sha = _obs._write_input_sidecar(
            ctx,
            seq,
            node.name,
            attempt,
            prompt,
            kind="shadow_codex_prompt",
        )
        shadow.prompt_path = prompt_path or ""
        shadow.prompt_sha256 = prompt_sha or ""
        if prompt_path:
            _obs._emit_event(
                ctx,
                "shadow_review_prompt",
                {
                    "node": node.name,
                    "attempt": str(attempt),
                    "shadow_backend": "codex",
                    "shadow_prompt_path": shadow.prompt_path,
                    "shadow_prompt_sha256": shadow.prompt_sha256,
                },
                seq,
            )
    except Exception:
        pass


def start_shadow_codex_review(
    node,
    ctx,
    *,
    workdir=None,
) -> _ShadowCodexReview | None:
    """Launch the parallel plain-Codex shadow review.

    The ``primary_prompt`` is taken from ``ctx.state['_last_output']``
    so the helper mirrors the original codergen signature. Set
    ``workdir=None`` to fall back to the base ``_sandboxed_args``
    (no sealed-doc deny rules); supply the *implementing-agent*
    worktree to use ``_sandboxed_args_for_workdir`` which adds
    the jleechan-113 sealed-doc deny rules.
    """
    if not _enabled(node, ctx):
        return None

    prompt = _build_prompt(node, ctx, str(ctx.state.get("_last_output", "")))
    shadow = _ShadowCodexReview(prompt=prompt, started_at=time.monotonic())
    _emit_prompt_sidecar(shadow, node, ctx, prompt)

    if shutil.which("codex") is None:
        shadow.launch_error = "codex executable not found"
        return shadow

    base_args = [
        "codex",
        "exec",
        "--yolo",
        "--skip-git-repo-check",
        prompt,
    ]
    if workdir is not None:
        args = _handlers_shim._sandboxed_args_for_workdir(base_args, workdir)
    else:
        args = _handlers_shim._sandboxed_args(base_args)
    if args is None:
        shadow.launch_error = "sandbox-exec unavailable"
        return shadow
    try:
        shadow.proc = subprocess.Popen(
            args,
            cwd=workdir if workdir is not None else ctx.workdir,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
            env=_handlers_shim._sanitized_env(),
        )
    except Exception as exc:
        shadow.launch_error = f"{type(exc).__name__}: {exc}"
    return shadow


def finish_shadow_codex_review(
    result,
    shadow: _ShadowCodexReview | None,
    node,
    ctx,
    *,
    expected_sha: str | None = None,
):
    """Merge the parallel Codex review into a reviewer node's result.

    ``expected_sha=None`` (default) preserves the historical codergen
    contract: no SHA-echo parity check. ``expected_sha="<sha>"``
    activates ``_verify_head_sha_echo``; mismatch drops a normalized
    ``success``/``unknown`` shadow outcome to ``error`` (the dispatch
    gate behavior, kept opt-in so this helper can serve both lanes).
    """
    if shadow is None:
        return result

    timeout_s = _handlers_shim._coerce_timeout(
        node.attrs.get("shadow_codex_timeout", node.attrs.get("timeout", "1200")),
        1200,
    )
    stdout = ""
    stderr = ""
    timed_out = False
    returncode = ""
    head_sha_status = "missing"
    if shadow.launch_error:
        output = f"shadow codex review did not run: {shadow.launch_error}"
        shadow_outcome = "error"
        verdict = "unknown"
    else:
        proc = shadow.proc
        if proc is None:
            output = "shadow codex review did not run: missing process handle"
            shadow_outcome = "error"
            verdict = "unknown"
        else:
            remaining = max(1, timeout_s - int(time.monotonic() - shadow.started_at))
            try:
                stdout, stderr = proc.communicate(timeout=remaining)
            except subprocess.TimeoutExpired:
                timed_out = True
                # killpg cascade: SIGTERM first, then SIGKILL on stuck process.
                # This is the codergen-specific behavior we must NOT regress —
                # the dispatch gate helper only does proc.kill() on timeout.
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                    stdout, stderr = proc.communicate(timeout=5)
                except Exception:
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except Exception:
                        pass
                    stdout, stderr = proc.communicate()
            returncode = str(proc.returncode if proc.returncode is not None else "")
            output = (stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip()
            if timed_out and not output:
                output = f"shadow codex review timed out after {timeout_s} seconds"
            verdict, normalized = _handlers_shim._parse_verdict(output)
            sha_ok, observed_sha = True, ""
            if expected_sha:
                sha_ok, observed_sha = _handlers_shim._verify_head_sha_echo(output, expected_sha)
                head_sha_status = (
                    "matched" if sha_ok and observed_sha
                    else ("mismatched" if observed_sha else "missing")
                )
            if proc.returncode != 0 or timed_out:
                shadow_outcome = "error"
            elif expected_sha and not sha_ok and normalized in {"success", "unknown"}:
                shadow_outcome = "error"
            else:
                shadow_outcome = normalized

    output_path = ""
    output_sha = ""
    try:
        from . import engine_observability as _obs

        seq = int(getattr(ctx, "_df_current_seq", getattr(ctx, "last_completed_seq", 0)))
        attempt = int(getattr(ctx, "_df_current_attempt", 1))
        output_path, output_sha = _obs._write_input_sidecar(
            ctx,
            seq,
            node.name,
            attempt,
            output,
            kind="shadow_codex_output",
        )
        _obs._emit_event(
            ctx,
            "shadow_review_result",
            {
                "node": node.name,
                "attempt": str(attempt),
                "shadow_backend": "codex",
                "shadow_outcome": shadow_outcome,
                "shadow_verdict": verdict,
                "shadow_returncode": returncode,
                "shadow_output_path": output_path or "",
                "shadow_output_sha256": output_sha or "",
            },
            seq,
        )
    except Exception:
        pass

    meta = dict(result.metadata)
    meta.update(
        {
            "shadow_codex_review": "true",
            "shadow_codex_outcome": shadow_outcome,
            "shadow_codex_verdict": verdict,
            "shadow_codex_returncode": returncode,
            "shadow_codex_timed_out": "true" if timed_out else "false",
            "shadow_codex_prompt_path": shadow.prompt_path,
            "shadow_codex_prompt_sha256": shadow.prompt_sha256,
            "shadow_codex_output_path": output_path or "",
            "shadow_codex_output_sha256": output_sha or "",
        }
    )
    if expected_sha:
        # Dispatch-shape parity metadata; only emitted when the gate
        # contract is active. Codergen (expected_sha=None) leaves
        # these keys absent, preserving the historical wire format.
        meta["shadow_codex_head_sha_status"] = head_sha_status

    comparison = (
        "\n\n---\n\n"
        "## Parallel Codex Review\n"
        f"{output}\n\n"
        "## Review Comparison\n"
        f"- Primary reviewer outcome: {result.outcome}\n"
        f"- Shadow Codex outcome: {shadow_outcome}\n"
        f"- Shadow Codex verdict: {verdict}\n"
    )
    final_outcome = result.outcome
    if result.outcome == "success" and shadow_outcome != "success":
        final_outcome = "failure"
    updates = dict(result.context_updates)
    updates[f"{node.name}.shadow_codex_output"] = output
    updates[f"{node.name}.shadow_codex_outcome"] = shadow_outcome
    updates[f"{node.name}.shadow_codex_output_path"] = output_path or ""
    return type(result)(
        outcome=final_outcome,
        output=result.output + comparison,
        metadata=meta,
        preferred_label=result.preferred_label,
        suggested_next_ids=result.suggested_next_ids,
        context_updates=updates,
    )
