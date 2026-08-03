"""Gate subprocess dispatch + adversarial-review priority queue.

Owns:
  * `_gate_subprocess_args` — build sandboxed argv for agy/codex/minimax/claude
    per backend.
  * `_gate_subprocess_env` — add ``ANTHROPIC_BASE_URL`` for minimax; layer on
    ``_sanitized_env``.
  * `_run_gate_once` — run one gate attempt, parse verdict, SHA-bind,
    classify outcome.
  * `_is_gate_infra_failure` — detect sandbox/timeout/missing-binary vs real
    verdict.
  * `_DEFAULT_ADVERSARIAL_PRIORITY` — default queue
    ``["codex", "minimax", "agy", "claude-sonnet"]``.
  * `_parse_priority_env` — parse ``DARK_FACTORY_ADVERSARIAL_PRIORITY``.
  * `_probe_backend_installed` — ``which <name>`` + ``<name> --version`` with
    5s ceiling.
  * `_resolve_adversarial_backend` — pick first installed from priority
    queue; metadata audit.
  * `_resolve_gate_backend` — resolve node-level priority OR explicit backend
    OR run-level; cross-visit pin.
  * `_coerce_bool_attr` — truthy/falsy parser for DOT attributes.
  * `_execute_gate` — run gate; agy→claude infra fallback (no
    reviewer-shopping).

All monkeypatched helper symbols are looked up via ``runner.handlers``
(late binding) so that
``monkeypatch.setattr("runner.handlers._sandboxed_args", ...)`` and friends
still take effect.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Optional

# ``runner.handlers`` re-imports symbols from this module at module load
# (TYPE_REGISTRY registration), so importing it eagerly here would form a
# cycle. Use the lazy ``import runner.handlers as _handlers_shim`` shim
# inside each consumer function (see PR #503 fix).

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


@dataclass
class _ShadowGateReview:
    prompt: str = ""
    proc: subprocess.Popen | None = None
    prompt_path: str = ""
    prompt_sha256: str = ""
    launch_error: str = ""
    started_at: float = 0.0
    backend: str = "codex"


def _resolve_shadow_backend_env() -> str:
    """Resolve the override shadow backend from DARK_FACTORY_SHADOW_BACKEND env var.

    Ironclad #159: lets pilots launched from launchd/cron override the
    shadow backend without patching the dot file. Validation: only known
    backends (codex, minimax, agy, claude-sonnet, claude) are honored;
    empty or unknown values fall back to ``codex``.
    """
    raw = os.environ.get("DARK_FACTORY_SHADOW_BACKEND", "").strip().lower()
    if not raw:
        return "codex"
    if raw not in {"codex", "minimax", "agy", "claude-sonnet", "claude"}:
        return "codex"
    return raw


def _shadow_gate_enabled(ctx: "Context") -> bool:
    raw = ctx.state.get("_df_shadow_codex_review", "false")
    if isinstance(raw, str):
        return raw.strip().lower() in {"true", "1", "yes", "on"}
    return bool(raw)


def _shadow_gate_prompt(name: str, prompt: str, expected_sha: str, ctx: "Context") -> str:
    target = "evidence" if name in {"gate_es", "gate_er", "es", "er", "evidence_review"} else "diff"
    return f"""\
review this {target}

You are the parallel plain-Codex reviewer for a Dark Factory gate.
Review the current workspace independently from the normal gate prompt.
Focus on blocker-first findings a coder can act on next.

Goal:
{ctx.goal}

Gate:
{name}

Expected HEAD SHA:
{expected_sha}

Normal gate prompt for comparison:
```
{prompt}
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

Include this line near the top:
head_sha: {expected_sha}

End with this machine-readable routing line:
verdict: <pass|fail>
"""


def _launch_shadow_gate_review(
    name: str,
    prompt: str,
    expected_sha: str,
    timeout: int,
    ctx: "Context",
    backend: str = "codex",
) -> _ShadowGateReview | None:
    import runner.handlers as _handlers_shim  # late-bound shim
    """Spawn a shadow reviewer on ``backend`` (no enable-gate).

    This is the generalized spawn body; callers must perform their own
    enable check (see :func:`_start_shadow_gate_review` for the single-codex
    back-compat wrapper, or :func:`handler_parallel_reviewer._parallel_reviewer`
    for the N-way lane orchestrator). Returns the populated
    :class:`_ShadowGateReview` even when launch fails — failures are recorded
    via ``launch_error`` so the caller can surface them as ``shadow_outcome``
    rather than masking them.

    Ironclad #159: when the caller didn't override ``backend``
    (still the default "codex"), honor the ``DARK_FACTORY_SHADOW_BACKEND``
    env var so launchd/cron-launched pilots can override the shadow
    backend without patching the dot file. Caller-specified non-default
    backends always win.
    """
    if backend == "codex":
        backend = _resolve_shadow_backend_env()
    shadow_prompt = _shadow_gate_prompt(name, prompt, expected_sha, ctx)
    shadow = _ShadowGateReview(prompt=shadow_prompt, started_at=time.monotonic(), backend=backend)
    seq = int(getattr(ctx, "_df_current_seq", getattr(ctx, "last_completed_seq", 0)))
    attempt = int(getattr(ctx, "_df_current_attempt", 1))
    node_name = str(getattr(ctx, "_df_current_node", name))
    try:
        from . import engine_observability as _obs

        prompt_path, prompt_sha = _obs._write_input_sidecar(
            ctx,
            seq,
            node_name,
            attempt,
            shadow_prompt,
            kind=f"shadow_{backend}_gate_prompt",
        )
        shadow.prompt_path = prompt_path or ""
        shadow.prompt_sha256 = prompt_sha or ""
        if prompt_path:
            _obs._emit_event(
                ctx,
                "shadow_gate_prompt",
                {
                    "node": node_name,
                    "attempt": str(attempt),
                    "shadow_backend": backend,
                    "shadow_prompt_path": shadow.prompt_path,
                    "shadow_prompt_sha256": shadow.prompt_sha256,
                },
                seq,
            )
    except Exception:
        pass
    probe_bin = "codex" if backend == "codex" else ("agy" if backend == "agy" else _handlers_shim._get_claude_executable())
    if shutil.which(probe_bin) is None:
        shadow.launch_error = f"{backend} executable not found"
        return shadow
    args = _gate_subprocess_args(backend, shadow_prompt, ctx, timeout)
    if args is None:
        shadow.launch_error = "sandbox-exec unavailable"
        return shadow
    try:
        shadow.proc = subprocess.Popen(
            args,
            cwd=ctx.workdir,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
            env=_gate_subprocess_env(backend),
        )
    except Exception as exc:
        shadow.launch_error = f"{type(exc).__name__}: {exc}"
    return shadow


def _start_shadow_gate_review(
    name: str,
    prompt: str,
    expected_sha: str,
    timeout: int,
    ctx: "Context",
    backend: str = "codex",
) -> _ShadowGateReview | None:
    """Back-compat wrapper: gate-on-enable then spawn the shadow reviewer.

    Preserves the legacy ``_df_shadow_codex_review`` enable flag and the
    single-codex default. New code should prefer the N-way
    ``handler_parallel_reviewer._parallel_reviewer`` orchestrator instead.
    """
    if not _shadow_gate_enabled(ctx):
        return None
    return _launch_shadow_gate_review(name, prompt, expected_sha, timeout, ctx, backend)


def _finish_shadow_gate_review(
    result: "Result",
    shadow: _ShadowGateReview | None,
    name: str,
    expected_sha: str,
    timeout: int,
    ctx: "Context",
) -> "Result":
    import runner.handlers as _handlers_shim  # late-bound shim
    if shadow is None:
        return result

    backend = shadow.backend or "codex"
    prefix = f"shadow_{backend}_gate_"
    label = backend.capitalize()

    returncode = ""
    timed_out = False
    if shadow.launch_error:
        output = f"shadow codex gate review did not run: {shadow.launch_error}"
        verdict = "unknown"
        shadow_outcome = "error"
        head_sha_status = "missing"
    else:
        proc = shadow.proc
        if proc is None:
            output = "shadow codex gate review did not run: missing process handle"
            verdict = "unknown"
            shadow_outcome = "error"
            head_sha_status = "missing"
        else:
            remaining = max(1, timeout - int(time.monotonic() - shadow.started_at))
            try:
                stdout, stderr = proc.communicate(timeout=remaining)
            except subprocess.TimeoutExpired:
                timed_out = True
                proc.kill()
                stdout, stderr = proc.communicate()
            returncode = str(proc.returncode if proc.returncode is not None else "")
            output = (stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip()
            if timed_out and not output:
                output = f"shadow codex gate review timed out after {timeout} seconds"
            verdict, normalized = _handlers_shim._parse_verdict(output)
            sha_ok, observed_sha = _handlers_shim._verify_head_sha_echo(output, expected_sha)
            head_sha_status = (
                "matched" if sha_ok and observed_sha
                else ("mismatched" if observed_sha else "missing")
            )
            if proc.returncode != 0 or timed_out:
                shadow_outcome = "error"
            elif not sha_ok and normalized in {"success", "unknown"}:
                shadow_outcome = "error"
            else:
                shadow_outcome = normalized

    output_path = ""
    output_sha = ""
    seq = int(getattr(ctx, "_df_current_seq", getattr(ctx, "last_completed_seq", 0)))
    attempt = int(getattr(ctx, "_df_current_attempt", 1))
    node_name = str(getattr(ctx, "_df_current_node", name))
    try:
        from . import engine_observability as _obs

        output_path, output_sha = _obs._write_input_sidecar(
            ctx,
            seq,
            node_name,
            attempt,
            output,
            kind=f"shadow_{backend}_gate_output",
        )
        _obs._emit_event(
            ctx,
            "shadow_gate_result",
            {
                "node": node_name,
                "attempt": str(attempt),
                "shadow_backend": backend,
                "shadow_outcome": shadow_outcome,
                "shadow_verdict": verdict,
                "shadow_returncode": returncode,
                "shadow_head_sha_status": head_sha_status,
                "shadow_output_path": output_path or "",
                "shadow_output_sha256": output_sha or "",
            },
            seq,
        )
    except Exception:
        pass

    metadata = dict(result.metadata)
    metadata.update(
        {
            f"{prefix}review": "true",
            f"{prefix}outcome": shadow_outcome,
            f"{prefix}verdict": verdict,
            f"{prefix}returncode": returncode,
            f"{prefix}head_sha_status": head_sha_status,
            f"{prefix}timed_out": "true" if timed_out else "false",
            f"{prefix}prompt_path": shadow.prompt_path,
            f"{prefix}prompt_sha256": shadow.prompt_sha256,
            f"{prefix}output_path": output_path or "",
            f"{prefix}output_sha256": output_sha or "",
        }
    )
    comparison = (
        "\n\n---\n\n"
        f"## Parallel {label} Gate Review\n"
        f"{output}\n\n"
        "## Gate Review Comparison\n"
        f"- Normal gate outcome: {result.outcome}\n"
        f"- Normal gate verdict: {result.metadata.get('verdict', 'unknown')}\n"
        f"- Shadow {label} outcome: {shadow_outcome}\n"
        f"- Shadow {label} verdict: {verdict}\n"
        f"- Shadow {label} head_sha_status: {head_sha_status}\n"
    )
    final_outcome = result.outcome
    if result.outcome == "success" and shadow_outcome != "success":
        final_outcome = "failure"
    updates = dict(result.context_updates)
    updates[f"{name}.{prefix}output"] = output
    updates[f"{name}.{prefix}outcome"] = shadow_outcome
    updates[f"{name}.{prefix}output_path"] = output_path or ""
    return Result(
        outcome=final_outcome,
        output=result.output + comparison,
        metadata=metadata,
        preferred_label=result.preferred_label,
        suggested_next_ids=result.suggested_next_ids,
        context_updates=updates,
    )


def _gate_subprocess_args(backend: str, prompt: str, ctx: "Context", timeout: int) -> Optional[list[str]]:
    import runner.handlers as _handlers_shim  # late-bound shim
    """Build the sandboxed argv for a *reviewer* gate on the given backend.

    Supported backends:
      - ``agy`` — Google Antigravity / Gemini CLI. Gets ``--add-dir`` so it
        can read the diff/evidence in the worktree but never enters planning
        mode.
      - ``codex`` — OpenAI Codex CLI (``codex exec --yolo``).
      - ``minimax`` — Anthropic Claude CLI routed through the minimax
        gateway (env override handled by ``_gate_subprocess_env``).
      - ``claude-sonnet`` (or bare ``claude``) — Anthropic Claude CLI.

    The historical default mapped every non-``agy`` name to ``claude``; that
    made the adversarial-review priority queue decorative (a resolved
    ``codex`` still ran the Claude subprocess). The dispatch now honors the
    resolved name end-to-end so cross-vendor review is a real subprocess
    rather than a metadata label.

    Returns ``None`` when sandbox-exec is unavailable.
    """
    if backend == "agy":
        return _handlers_shim._sandboxed_args([
            "agy",
            "--add-dir", str(ctx.workdir),
            "--dangerously-skip-permissions",
            "--print-timeout", f"{timeout}s",
            "--print",
            prompt,
        ])
    if backend == "codex":
        return _handlers_shim._sandboxed_args([
            "codex", "exec", "--yolo", "--skip-git-repo-check", prompt,
        ])
    # ``claude-sonnet`` (priority-queue name), bare ``claude`` (run-level
    # default), and any other claude-routed backend → Anthropic Claude CLI.
    # ``minimax`` is a special case of this path with a different base URL
    # (see ``_gate_subprocess_env``).
    claude_bin = _handlers_shim._get_claude_executable()
    return _handlers_shim._sandboxed_args([claude_bin, "--print", "--dangerously-skip-permissions", prompt])


def _gate_subprocess_env(backend: str) -> dict[str, str]:
    import runner.handlers as _handlers_shim  # late-bound shim
    """Env overrides for a reviewer-gate subprocess on ``backend``.

    For ``minimax`` the Claude CLI must route through the minimax Anthropic-
    compatible gateway; ``ANTHROPIC_BASE_URL`` is the only override, layered
    on top of ``_sanitized_env`` (never raw ``os.environ`` — holdout vars
    must not reach any reviewer subprocess). All other backends use
    ``_sanitized_env`` unchanged.
    """
    if backend == "minimax":
        return {**_handlers_shim._sanitized_env(), "ANTHROPIC_BASE_URL": "https://api.minimax.io/anthropic"}
    return _handlers_shim._sanitized_env()


def _controller_codex_args(args: list[str]) -> list[str]:
    """Build the controller Codex transport argv.

    Preserves any ``sandbox-exec -p ...`` wrapper prefix and inserts the
    controller transport flags (``--json --ephemeral --skip-git-repo-check
    --sandbox read-only``) while replacing the prompt positional with
    ``-`` (stdin). Fails closed on unsafe transport options so the
    controller never launches a write-capable Codex subprocess.
    """
    prepared = list(args)
    if not prepared:
        raise ValueError("codex controller argv is empty")

    # Locate the codex binary within the argv (skip any sandbox-exec wrapper).
    try:
        codex_index = next(
            index
            for index, value in enumerate(prepared)
            if pathlib.Path(value).name == "codex"
        )
    except StopIteration as exc:
        raise ValueError("codex executable missing from reviewer argv") from exc

    codex_args = prepared[codex_index:]
    if len(codex_args) < 3:
        raise ValueError("codex controller command missing review payload")
    if codex_args[1] != "exec":
        raise ValueError("controller transport requires 'codex exec'")

    # Validate transport safety before reconstructing the argv.
    for idx, value in enumerate(codex_args):
        if value == "--dangerously-bypass-approvals-and-sandbox":
            raise ValueError("controller transport uses unsafe codex flags")
        if value.startswith("--sandbox="):
            mode = value.split("=", 1)[1].strip().lower()
            if mode != "read-only":
                raise ValueError("controller transport requires read-only codex sandboxing")
        elif value == "--sandbox":
            if idx + 1 >= len(codex_args):
                raise ValueError("codex controller command uses invalid --sandbox mode")
            mode = codex_args[idx + 1].strip().lower()
            if mode != "read-only":
                raise ValueError("controller transport requires read-only codex sandboxing")

    # Preserve everything up to and including "codex exec".
    prefix = prepared[:codex_index + 2]

    # Strip transport flags we replace and the original prompt positional.
    codex_tail = codex_args[2:]
    safe_tail: list[str] = []
    skip_next = False
    for idx, value in enumerate(codex_tail):
        if skip_next:
            skip_next = False
            continue
        if value in ("--yolo", "--json", "--ephemeral", "--skip-git-repo-check"):
            continue
        if value == "--sandbox":
            skip_next = True
            continue
        if value.startswith("--sandbox="):
            continue
        # Drop the original trailing prompt positional (last non-flag arg).
        if idx == len(codex_tail) - 1 and not value.startswith("-"):
            continue
        safe_tail.append(value)

    return prefix + [
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        *safe_tail,
        "-",
    ]


def _run_gate_once(
    backend: str, prompt: str, expected_sha: str, timeout: int, ctx: "Context", name: str,
    *, gate_strict: bool = False,
) -> "Result":
    import runner.handlers as _handlers_shim  # late-bound shim
    """Run one reviewer-gate attempt on ``backend`` and classify the result.

    SHA binding, verdict parsing, and infra-vs-real-failure classification are
    identical across backends, so the only backend-specific parts are the
    argv (built by ``_gate_subprocess_args``) and the env (built by
    ``_gate_subprocess_env``). ``reviewer_backend`` is recorded in metadata
    so the operator/CXDB can see what actually graded the diff — the
    recorded name matches the resolved priority-queue name end-to-end
    (e.g. ``codex`` means a codex subprocess really ran, not just a label).
    """
    # The recorded name must match the subprocess that actually ran. agy is
    # passed through as-is; minimax is recorded as ``minimax`` even though it
    # invokes the Claude CLI (the review is graded by the minimax-routed
    # model, which is the cross-vendor intent). Everything else is whatever
    # the priority queue / run-level config chose.
    reviewer_backend = backend
    prompt_meta: dict[str, str] = {}
    try:
        from . import engine_observability as _obs

        seq = int(getattr(ctx, "_df_current_seq", getattr(ctx, "last_completed_seq", 0)))
        attempt = int(getattr(ctx, "_df_current_attempt", 1))
        node_name = str(getattr(ctx, "_df_current_node", name))
        prompt_path, prompt_sha = _obs._write_input_sidecar(
            ctx,
            seq,
            node_name,
            attempt,
            prompt,
            kind="llm_prompt",
        )
        if prompt_path:
            prompt_meta = {
                "llm_prompt_path": prompt_path,
                "llm_prompt_sha256": prompt_sha or "",
            }
            _obs._emit_event(
                ctx,
                "node_prompt",
                {
                    "node": node_name,
                    "attempt": str(attempt),
                    **prompt_meta,
                },
                seq,
            )
    except Exception:
        prompt_meta = {}
    sub_args = _gate_subprocess_args(backend, prompt, ctx, timeout)
    sub_env = _gate_subprocess_env(backend)
    if sub_args is None:
        return Result(
            outcome="failure",
            output="sandbox-exec unavailable",
            metadata={"slash_command": name, "verdict": "unknown",
                      "reviewer_backend": reviewer_backend, "sandbox": "unavailable",
                      **prompt_meta},
        )
    shadow_review = _start_shadow_gate_review(name, prompt, expected_sha, timeout, ctx)

    def _finalize(result: "Result") -> "Result":
        return _finish_shadow_gate_review(result, shadow_review, name, expected_sha, timeout, ctx)

    # agy enforces its own --print-timeout; give the outer wait a small buffer
    # so we read agy's timeout message rather than killing it first.
    run_timeout = timeout + 30 if backend == "agy" else timeout
    try:
        proc = subprocess.run(
            sub_args, cwd=ctx.workdir, capture_output=True, text=True,
            timeout=run_timeout, check=False, env=sub_env,
        )
    except subprocess.TimeoutExpired as exc:
        # TimeoutExpired carries bytes for stdout/stderr even when the run
        # used text=True — coerce before concatenating.
        def _as_text(v: "str | bytes | None") -> str:
            if v is None:
                return ""
            if isinstance(v, bytes):
                return v.decode("utf-8", errors="replace")
            return v

        combined = _as_text(exc.stdout) + "\n" + _as_text(exc.stderr)
        return _finalize(Result(
            outcome="failure",
            output=combined.strip() or f"gate {name} timed out after {run_timeout}s",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "timed_out": "true",
                      "reviewer_backend": reviewer_backend, **prompt_meta},
        ))
    except FileNotFoundError as exc:
        return _finalize(Result(
            outcome="error",
            output=f"gate {name} backend {reviewer_backend!r} not found: {exc}",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "reviewer_backend": reviewer_backend,
                      "backend_missing": "true", **prompt_meta},
        ))
    except Exception as exc:
        return _finalize(Result(
            outcome="error",
            output=f"gate {name} subprocess failed: {exc}",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "reviewer_backend": reviewer_backend,
                      **prompt_meta},
        ))
    combined = proc.stdout + "\n" + proc.stderr
    verdict, normalized = _handlers_shim._parse_verdict(combined, gate_strict=gate_strict)
    # SHA binding check comes BEFORE collapsing to pass/fail so a spoofed-pass
    # with the wrong SHA collapses to `error`, not `success`.
    sha_ok, observed_sha = _handlers_shim._verify_head_sha_echo(combined, expected_sha)
    if proc.returncode != 0 and (verdict == "unknown" or normalized == "success"):
        outcome = "error"
    elif not sha_ok:
        # Spoofed PASS / unknown without a SHA echo → error. A real FAIL/PARTIAL
        # without a SHA echo is kept (conservative — never hide a real verdict).
        outcome = "error" if normalized in ("success", "unknown") else normalized
    else:
        outcome = normalized
    head_sha_status = (
        "matched" if sha_ok and observed_sha
        else ("mismatched" if observed_sha else "missing")
    )
    context_updates = {}
    if outcome == "success":
        context_updates["_last_validated_head_sha"] = expected_sha
    return _finalize(Result(
        outcome=outcome,
        output=proc.stdout,
        metadata={
            "slash_command": name, "verdict": verdict,
            "returncode": str(proc.returncode),
            "expected_head_sha": expected_sha, "observed_head_sha": observed_sha,
            "head_sha_status": head_sha_status,
            "reviewer_backend": reviewer_backend,
            **prompt_meta,
        },
        context_updates=context_updates,
    ))


def _is_gate_infra_failure(result: "Result") -> bool:
    """True when a gate result is an *infrastructure* failure (not a real verdict).

    Only infra failures justify the agy→claude fallback. A genuine
    ``verdict: fail|partial`` is a real review result and must never trigger a
    retry on a different backend (that would be reviewer-shopping).
    """
    if result.outcome == "error":
        return True
    md = result.metadata or {}
    return md.get("sandbox") == "unavailable" or md.get("timed_out") == "true" or md.get("backend_missing") == "true"


# Default adversarial-review priority queue. Read at run-config time
# (DARK_FACTORY_ADVERSARIAL_PRIORITY env var, comma-separated); chosen for the
# whole run, NOT a retry cascade. A real fail|partial from one reviewer is
# authoritative and must never be retried on a different model — see
# feedback_2026-05-31_runner_resilience_reviewer_gates.md for the
# no-reviewer-shopping rule.
_DEFAULT_ADVERSARIAL_PRIORITY = ["codex", "minimax", "agy", "claude-sonnet"]


def _parse_priority_env(raw: str) -> list[str]:
    """Parse a comma-separated priority list from the env var. Whitespace and
    empty entries are stripped. Order is preserved (left = highest priority).
    """
    out: list[str] = []
    for entry in raw.split(","):
        name = entry.strip()
        if name:
            out.append(name)
    return out


def _probe_backend_installed(name: str) -> bool:
    """True when ``<name>`` is on PATH and responds to ``--version``.

    The probe is intentionally cheap (which + a quick --version) so that the
    resolver can be called from gate dispatch without adding noticeable
    latency. A backend that hangs on --version would block; we rely on the
    existing ``subprocess.run(timeout=...)`` envelope in ``_run_gate_once`` to
    catch the hang, but the probe itself uses a 5s ceiling.
    """
    bin_path = shutil.which(name)
    if not bin_path:
        return False
    try:
        proc = subprocess.run(
            [bin_path, "--version"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
        return False
    return proc.returncode == 0


def _resolve_adversarial_backend(
    priority: list[str] | None,
    ctx: "Context",
) -> tuple[str, dict[str, str]]:
    import runner.handlers as _handlers_shim  # late-bound shim
    """Pick the first installed backend from the adversarial priority queue.

    Resolution order:
      1. Per-call ``priority`` argument (e.g. from a ``backend_priority=...``
         node attribute) — the lane-specified queue for this gate.
      2. ``DARK_FACTORY_ADVERSARIAL_PRIORITY`` env var, comma-separated.
      3. ``_DEFAULT_ADVERSARIAL_PRIORITY`` (the dark-factory default).

    Each entry is probed (``which <name>`` + ``<name> --version``); the first
    one that responds is returned. The returned tuple is
    ``(backend_name, metadata)`` where ``metadata`` records the priority
    list, the resolved backend, and the entries that were skipped because
    they are not installed. The metadata is meant to be merged into the gate
    ``Result.metadata`` so the operator/CXDB can see why a particular backend
    was picked (or why the resolver fell all the way through to claude-sonnet).

    This is the FIRST adversarial pass selector — *not* a retry cascade. A
    real fail|partial from the chosen backend is kept (the no-reviewer-shopping
    rule is load-bearing in ``_execute_gate``).
    """
    raw = os.environ.get("DARK_FACTORY_ADVERSARIAL_PRIORITY", "")
    if raw:
        priority = _parse_priority_env(raw)
    elif priority is None:
        priority = list(_DEFAULT_ADVERSARIAL_PRIORITY)
    else:
        priority = [str(p) for p in priority if p]

    skipped: list[str] = []
    resolved: str | None = None
    for name in priority:
        if _handlers_shim._probe_backend_installed(name):
            resolved = name
            break
        skipped.append(name)

    # Fall through to the last entry even if uninstalled (the gate machinery
    # will report backend_missing=true, which is a real infra failure that
    # _execute_gate can route to claude on agy, or surface honestly otherwise).
    # This keeps "nothing installed" honest: the resolver still returns a
    # named backend so the gate runs, the gate's missing-binary path fires,
    # and the operator sees the full skip list in metadata.
    if resolved is None:
        resolved = priority[-1] if priority else _DEFAULT_ADVERSARIAL_PRIORITY[-1]

    meta = {
        "adversarial_priority": ",".join(priority),
        "adversarial_resolved": resolved,
        "adversarial_skipped": ",".join(skipped),
    }
    return resolved, meta


def _resolve_gate_backend(node: "Node", ctx: "Context") -> tuple[str, dict[str, str]]:
    """Resolve the reviewer backend for a gate node.

    Resolution order:
      1. ``backend_priority=...`` node attribute — adversarial-review queue.
         Triggers ``_resolve_adversarial_backend``; the first installed entry
         wins. With ``prefer_adversarial: true`` the run-level coder backend
         is also skipped so the reviewer is always a different vendor.
      2. Explicit per-node ``backend`` attr (set directly or by a ``.review``
         model-stylesheet rule, e.g. ``backend: agy``) — wins over the
         run-level ``ctx.backend``.
      3. Run-level ``ctx.backend``.

    Returns ``(backend_name, metadata)``. ``metadata`` is the priority-queue
    audit trail (priority list, resolved name, skipped entries, and the
    prefer_adversarial flag) when ``backend_priority`` was used, else
    ``{"reviewer_backend_resolution": "explicit"}`` or
    ``{"reviewer_backend_resolution": "run_level"}``. Callers merge this into the
    gate ``Result.metadata`` so the operator/CXDB can see exactly why a
    particular backend was picked.
    """
    bp = node.attrs.get("backend_priority")
    if bp:
        priority = [p.strip() for p in str(bp).split(",") if p.strip()]
        if priority:
            prefer_adversarial = _coerce_bool_attr(node.attrs.get("prefer_adversarial", False))
            # Cross-visit pin: once a node's reviewer backend has been
            # resolved via the priority queue, the same name resolves to the
            # same backend on every subsequent visit — even if the PATH
            # changes between visits (e.g. `codex` is uninstalled mid-run).
            # This honors the design-doc promise "the runner pins the
            # reviewer for the entire run" (see
            # `roadmap/agy-reviewer-and-base-dot-2026-06-09.md` §5.2 and
            # the no-reviewer-shopping rule in
            # `feedback_2026-06-09_adversarial_review_real_llm.md`).
            # The first-write-wins rule also means a *real* fail from one
            # backend is never re-tried on a different one — the gate keeps
            # the verdict, not the resolver.
            prior_key = f"{node.name}.resolved_backend"
            prior = ctx.state.get(prior_key)
            prior_meta = ctx.state.get(f"{node.name}.resolved_backend_meta") or {}
            if prior and prior_meta.get("reviewer_backend_resolution") == "priority_queue":
                return prior, prior_meta
            # When prefer_adversarial is set, exclude the run-level coder
            # backend from the priority list (so a `claude` run with an
            # `agy` coder cannot accidentally get a `claude` reviewer).
            if prefer_adversarial and ctx.backend and ctx.backend in priority:
                priority = [p for p in priority if p != ctx.backend]
            # Empty post-filter list (e.g. lane says ``backend_priority=agy``
            # and the coder is agy) must NOT short-circuit straight to
            # ``claude-sonnet`` — that would skip probing codex / minimax /
            # agy in the default queue and silently collapse cross-vendor
            # review back onto Anthropic. Fall back to the full default
            # priority so every entry gets a real ``which``/``--version`` probe.
            if not priority:
                priority = list(_DEFAULT_ADVERSARIAL_PRIORITY)
            resolved, pq_meta = _resolve_adversarial_backend(priority, ctx)
            ctx.state[prior_key] = resolved
            pq_meta["prefer_adversarial"] = "true" if prefer_adversarial else "false"
            pq_meta["reviewer_backend_resolution"] = "priority_queue"
            ctx.state[f"{node.name}.resolved_backend_meta"] = dict(pq_meta)
            return resolved, pq_meta
    if "backend" in node.attrs:
        return str(node.attrs["backend"]), {"reviewer_backend_resolution": "explicit"}
    return str(ctx.backend), {"reviewer_backend_resolution": "run_level"}


def _coerce_bool_attr(value: object) -> bool:
    """Parse common boolean spellings from a DOT attribute. ``True`` / ``"true"``
    / ``"1"`` / ``"yes"`` are truthy; everything else is falsy. Missing
    attributes resolve to ``False``.
    """
    if value is None:
        return False
    if isinstance(value, bool):
        return value
    if isinstance(value, (int, float)):
        return bool(value)
    if isinstance(value, str):
        return value.strip().lower() in ("true", "1", "yes", "on")
    return False


def _execute_gate(
    prompt: str, expected_sha: str, timeout: int, ctx: "Context", name: str, backend: str,
    *, gate_strict: bool = False,
) -> "Result":
    """Run a reviewer gate on ``backend``; infra failures fall back to agy, then claude.

    Routing rules:
      - Run the resolved backend. If the result is an *infrastructure*
        failure (missing binary, sandbox unavailable, timeout, unparseable
        output, SHA mismatch with no real verdict) and the backend is not
        already agy/claude-routed:
        1. If backend is not agy and not claude/claude-sonnet, fall back to ``agy``.
        2. If backend is agy, or if the agy fallback also suffers an infra failure,
           fall back to ``claude``.
      - A real ``fail``/``partial`` verdict from any backend is kept as-is
        (no-reviewer-shopping): only non-verdicts trigger the fallback.
      - Any result that is still an infra failure after routing carries
        ``verdict: infra_failure`` so operators and downstream conditions can
        distinguish "the reviewer never graded the diff" from a real FAIL.

    ``_run_gate_once`` is the single point that builds the argv + env per
    backend, so the dispatch is end-to-end: a priority-queue resolution of
    ``codex`` actually invokes the codex subprocess, with
    ``reviewer_backend: codex`` recorded in the result metadata.
    """
    result = _run_gate_once(backend, prompt, expected_sha, timeout, ctx, name, gate_strict=gate_strict)

    if _is_gate_infra_failure(result):
        fallback_backends = []
        if backend not in ("agy", "claude", "claude-sonnet"):
            fallback_backends.append("agy")
        if backend not in ("claude", "claude-sonnet"):
            fallback_backends.append("claude")

        current_result = result
        for fb_backend in fallback_backends:
            fallback = _run_gate_once(fb_backend, prompt, expected_sha, timeout, ctx, name, gate_strict=gate_strict)
            fallback.metadata["fallback_used"] = "true"
            fallback.metadata["fallback_from"] = backend
            current_result = fallback
            if not _is_gate_infra_failure(current_result):
                return current_result

        current_result.metadata["verdict"] = "infra_failure"
        return current_result

    result.metadata.setdefault("fallback_used", "false")
    return result
