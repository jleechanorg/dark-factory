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

The shim is imported lazily inside each helper function
(``import runner.handlers as _handlers_shim``) rather than at module load.
A top-level ``import runner.handlers`` would create a module-load cycle:
this file is imported by ``runner.handlers`` to populate TYPE_REGISTRY, and
a partial re-import would re-enter the shim before its symbols were bound.
By the time any helper function runs, ``runner.handlers`` is fully loaded
and the late import returns the populated module — including any test
monkeypatch on its attributes.
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Optional

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context

# Importing ``runner.handlers`` at module top would create a module-load
# cycle: this file is imported by ``runner.handlers`` (line 110 of handlers.py
# does ``from .handler_dispatch import ...``) and by ``handler_parallel_reviewer``
# (line 29). Loading this file triggers ``import runner.handlers``; if that
# re-enters ``runner.handlers`` before it's finished, the re-entrant
# ``from .handler_dispatch import _gate_subprocess_args`` fails because
# ``_gate_subprocess_args`` isn't bound yet.
#
# Instead, the shim is imported lazily inside each helper function (see the
# ``import runner.handlers as _handlers_shim`` lines in function bodies).
# By the time any helper runs, ``runner.handlers`` is fully populated —
# including any test monkeypatch on its attributes — and the late import
# returns the populated module.


@dataclass
class _ShadowGateReview:
    prompt: str = ""
    proc: subprocess.Popen | None = None
    prompt_path: str = ""
    prompt_sha256: str = ""
    launch_error: str = ""
    started_at: float = 0.0
    backend: str = "codex"
    prompt_is_complete: bool = False
    json_transport: bool = False
    transport_argv: tuple[str, ...] = ()
    transport_text: str = ""
    response_text: str = ""
    transport_receipt: dict[str, str] | None = None
    output_dir: pathlib.Path | None = None
    lane_name: str = ""
    controller_runtime_root: pathlib.Path | None = None
    controller_runtime_writable: pathlib.Path | None = None


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
    *,
    prompt_is_complete: bool = False,
    read_only_path: pathlib.Path | str | None = None,
    lane_name: str = "",
) -> _ShadowGateReview | None:
    import runner.handlers as _handlers_shim  # late-bound shim (see module docstring)
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
    configured_lane = lane_name or f"shadow_{backend}"
    if backend == "codex":
        backend = _resolve_shadow_backend_env()
    shadow_prompt = prompt if prompt_is_complete else _shadow_gate_prompt(
        name, prompt, expected_sha, ctx,
    )
    shadow = _ShadowGateReview(
        prompt=shadow_prompt,
        started_at=time.monotonic(),
        backend=backend,
        prompt_is_complete=prompt_is_complete,
        lane_name=configured_lane,
    )
    if prompt_is_complete:
        raw_lane_dirs = ctx.state.get("_df_controller_review_lane_dirs", "{}")
        try:
            lane_dirs = json.loads(str(raw_lane_dirs))
        except (TypeError, json.JSONDecodeError):
            lane_dirs = {}
        configured_path = lane_dirs.get(configured_lane)
        if isinstance(configured_path, str) and configured_path:
            shadow.output_dir = pathlib.Path(configured_path)
    if prompt_is_complete and backend != "codex":
        shadow.launch_error = "controller review requests require codex backend"
        return shadow
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
    runtime = None
    if prompt_is_complete and backend == "codex":
        try:
            runtime = _handlers_shim._create_controller_runtime()
            args = _controller_codex_args(
                args,
                read_only_path=read_only_path or ctx.workdir,
                writable_path=runtime.codex_home,
                schema_path=_handlers_shim._controller_output_schema(runtime.run_dir),
            )
        except (OSError, RuntimeError, ValueError) as exc:
            if runtime is not None:
                try:
                    _handlers_shim._cleanup_controller_runtime(runtime.run_dir)
                except Exception:  # noqa: BLE001, S110 - best-effort runtime cleanup
                    pass
            shadow.launch_error = str(exc)
            return shadow
        shadow.controller_runtime_root = runtime.run_dir
        shadow.controller_runtime_writable = runtime.codex_home
        shadow.json_transport = True
    shadow.transport_argv = tuple(str(arg) for arg in args)
    review_cwd = ctx.workdir
    if prompt_is_complete:
        request = ctx.state.get("_df_controller_review_request")
        if request is not None:
            target = json.loads(request.envelope_json)["target"]
            review_cwd = pathlib.Path(str(target["workspace_path"]))
    try:
        shadow.proc = subprocess.Popen(
            args,
            cwd=review_cwd,
            stdin=subprocess.PIPE if shadow.json_transport else None,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
            env=runtime.env if runtime is not None else _gate_subprocess_env(backend),
            pass_fds=getattr(args, "pass_fds", ()),
        )
        _handlers_shim._close_pinned_launcher_command(args)
    except Exception as exc:
        _handlers_shim._close_pinned_launcher_command(args)
        if runtime is not None:
            try:
                _handlers_shim._cleanup_controller_runtime(runtime.run_dir)
            except Exception:  # noqa: BLE001, S110 - best-effort runtime cleanup
                pass
        shadow.launch_error = f"{type(exc).__name__}: {exc}"
    return shadow


def _start_shadow_gate_review(
    name: str,
    prompt: str,
    expected_sha: str,
    timeout: int,
    ctx: "Context",
    backend: str = "codex",
    *,
    prompt_is_complete: bool = False,
    read_only_path: pathlib.Path | str | None = None,
    lane_name: str = "",
) -> _ShadowGateReview | None:
    """Back-compat wrapper: gate-on-enable then spawn the shadow reviewer.

    Preserves the legacy ``_df_shadow_codex_review`` enable flag and the
    single-codex default. New code should prefer the N-way
    ``handler_parallel_reviewer._parallel_reviewer`` orchestrator instead.
    """
    if not _shadow_gate_enabled(ctx):
        return None
    return _launch_shadow_gate_review(
        name,
        prompt,
        expected_sha,
        timeout,
        ctx,
        backend,
        prompt_is_complete=prompt_is_complete,
        read_only_path=read_only_path,
        lane_name=lane_name,
    )


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
    transport_receipt = None
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
                if shadow.json_transport:
                    stdout, stderr = proc.communicate(
                        input=shadow.prompt,
                        timeout=remaining,
                    )
                else:
                    stdout, stderr = proc.communicate(timeout=remaining)
            except subprocess.TimeoutExpired:
                timed_out = True
                # Adopt codergen's escalation (jleechan-txdh dedup parity):
                # SIGTERM the process group first, drain stdout, then SIGKILL.
                # Pre-dedup dispatch used only ``proc.kill()`` (SIGTERM-via-
                # terminate, no SIGKILL); the dedup brings dispatch in line
                # with the codergen shadow path so a single escalation rule
                # governs both.
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                except Exception:
                    pass
                try:
                    stdout, stderr = proc.communicate(timeout=5)
                except Exception:
                    try:
                        os.killpg(proc.pid, signal.SIGKILL)
                    except Exception:
                        pass
                    stdout, stderr = proc.communicate()
            returncode = str(proc.returncode if proc.returncode is not None else "")
            output = (stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip()
            command_receipts = ()
            transport_receipt = None
            transport_error = ""
            if shadow.json_transport and not timed_out and proc.returncode == 0:
                try:
                    from .review_controller import parse_tool_free_codex_jsonl

                    request = ctx.state.get("_df_controller_review_request")
                    output, _transport_receipt = parse_tool_free_codex_jsonl(
                        stdout, request=request
                    )
                    transport_receipt = {
                        "transport": _transport_receipt.transport,
                        "prompt_sha256": _transport_receipt.prompt_sha256,
                        "envelope_sha256": _transport_receipt.envelope_sha256,
                        "response_sha256": _transport_receipt.response_sha256,
                        "head_sha": _transport_receipt.head_sha,
                        "tree_sha": _transport_receipt.tree_sha,
                        "evidence_manifest_sha256": _transport_receipt.evidence_manifest_sha256,
                    }
                    command_receipts = ()
                except Exception as exc:
                    transport_error = str(exc)
                    output = stdout.strip()
            if timed_out and not output:
                output = f"shadow codex gate review timed out after {timeout} seconds"
            verdict, normalized = _handlers_shim._parse_verdict(output)
            sha_ok, observed_sha = _handlers_shim._verify_head_sha_echo(output, expected_sha)
            head_sha_status = (
                "matched" if sha_ok and observed_sha
                else ("mismatched" if observed_sha else "missing")
            )
            if proc.returncode != 0 or timed_out or transport_error:
                shadow_outcome = "error"
            elif not sha_ok and normalized in {"success", "unknown"}:
                shadow_outcome = "error"
            else:
                shadow_outcome = normalized
            shadow.transport_text = stdout
            shadow.response_text = output

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
            f"{prefix}command_receipts": [
                {
                    "command": item.command,
                    "exit_code": item.exit_code,
                    "output_sha256": item.output_sha256,
                }
                for item in command_receipts
            ] if "command_receipts" in locals() else [],
        }
    )
    if transport_receipt is not None:
        metadata[f"{prefix}transport_receipt"] = transport_receipt
    if shadow.controller_runtime_root is not None:
        metadata["_controller_runtime_root"] = str(shadow.controller_runtime_root)
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


def _gate_subprocess_args(
    backend: str,
    prompt: str,
    ctx: "Context",
    timeout: int,
    *,
    workdir: "Optional[pathlib.Path]" = None,
) -> Optional[list[str]]:
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

    When ``workdir`` is provided, the sealed-doc deny rules (jleechan-113
    contract) cover that worktree's benchmark docs. This matches the
    codergen shadow-review path (``_sandboxed_args_for_workdir``); without
    this extension the dispatch shadow could not honor the same isolation
    when running inside ``ctx.workdir``. ``workdir=None`` falls back to
    the legacy holdout-only deny rules — matching every existing
    caller's behavior.

    Returns ``None`` when sandbox-exec is unavailable.
    """
    sealed_args_builder = getattr(_handlers_shim, "_sandboxed_args_for_workdir", None)
    if workdir is not None and callable(sealed_args_builder):
        # Caller explicitly opts in to the sealed-doc deny rule (jleechan-113).
        if backend == "agy":
            return sealed_args_builder([
                "agy",
                "--add-dir", str(workdir),
                "--dangerously-skip-permissions",
                "--print-timeout", f"{timeout}s",
                "--print",
                prompt,
            ], workdir)
        if backend == "codex":
            return sealed_args_builder([
                "codex", "exec", "--yolo", "--skip-git-repo-check", prompt,
            ], workdir)
        claude_bin = _handlers_shim._get_claude_executable()
        return sealed_args_builder([claude_bin, "--print", "--dangerously-skip-permissions", prompt], workdir)
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


def _build_controller_codex_transport(
    args: list[str],
    *,
    read_only_path: pathlib.Path | str | None = None,
    writable_path: pathlib.Path | str | None = None,
    schema_path: pathlib.Path | str | None = None,
) -> list[str]:
    """Build the concrete controller transport command for Codex review.

    The controller transport is always JSON-over-stdin:
      - complete payload on stdin (`-`), never argv positionals
      - `codex exec --json --ephemeral --skip-git-repo-check -`
      - shell/unified-exec/browser/computer features disabled and web search off
      - one outer Landlock/Seatbelt profile with a workspace write denial
    Any unsupported transport mode (prompt-in-argv, bypass, write-capable
    sandbox mode, or weaker backend) fails closed before launch.
    """
    import runner.handlers as _handlers_shim  # late-bound shim

    if os.environ.get("DISABLE_SANDBOX"):
        raise ValueError(
            "controller transport refuses DISABLE_SANDBOX; holdout isolation is required"
        )

    prepared = list(args)
    if not prepared:
        raise ValueError("codex controller argv is empty")

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

    # Preserve an outer wrapper when it carries a path-specific denial. On
    # macOS, the outer Seatbelt profile is the single sandbox: augment it with
    # the workspace write boundary and use Codex's externally-sandboxed mode
    # so Codex does not try to apply a nested Seatbelt profile. On Linux, the
    # LD_PRELOAD prefix is not a sandbox and native Codex read-only remains
    # necessary. A permissive test/development wrapper has no isolation value
    # and is intentionally removed.
    outer = prepared[:codex_index]
    path_denial = any(
        "(deny file-read*" in value or value.startswith("DENY_PATHS=")
        for value in outer
    )
    controller_tail = [
        "exec",
        "--json",
        "--ephemeral",
        "--ignore-user-config",
        "--skip-git-repo-check",
        "--disable",
        "shell_tool",
        "--disable",
        "unified_exec",
        "--disable",
        "browser_use",
        "--disable",
        "computer_use",
        "--config",
        'web_search="disabled"',
        "--ignore-rules",
    ]
    if schema_path is not None:
        controller_tail.extend(["--output-schema", str(schema_path)])
    controller_tail.append("-")
    if sys.platform == "darwin" and path_denial:
        sandbox_index = next(
            (
                index
                for index, value in enumerate(outer)
                if (
                    value == "sandbox-exec"
                    or pathlib.Path(value).name == "sandbox-exec"
                )
            ),
            None,
        )
        if (
            sandbox_index is None
            or sandbox_index + 2 >= len(outer)
            or outer[sandbox_index + 1] != "-p"
        ):
            raise ValueError("controller macOS transport lacks a sandbox profile")
        profile_index = sandbox_index + 2
        outer = list(outer)
        outer[profile_index] = _handlers_shim._macos_read_only_profile(
            outer[profile_index], read_only_path, writable_path
        )
        executable = prepared[codex_index]
        return outer + [executable, *controller_tail]
    if sys.platform.startswith("linux") and path_denial:
        if read_only_path is None:
            raise ValueError("controller Linux transport requires a read-only target path")
        denied_paths: list[pathlib.Path] = []
        for value in outer:
            if value.startswith("DENY_PATHS="):
                denied_paths.extend(
                    pathlib.Path(item)
                    for item in value.split("=", 1)[1].split(":")
                    if item
                )
        if not denied_paths:
            raise ValueError("controller Linux transport lacks holdout denial paths")
        executable = prepared[codex_index]
        resolved_executable = (
            shutil.which(executable) if pathlib.Path(executable).name == "codex" else executable
        ) or executable
        executable_path = pathlib.Path(resolved_executable)
        runtime_paths = _handlers_shim._linux_codex_runtime_paths(executable_path)
        if runtime_paths is None:
            raise ValueError("controller Linux transport cannot resolve Codex runtime")
        landlock_prefix = _handlers_shim._linux_controller_sandbox_prefix(
            denied_paths=denied_paths,
            read_paths=[
                pathlib.Path(read_only_path),
                *runtime_paths,
                pathlib.Path(schema_path) if schema_path is not None else pathlib.Path(read_only_path),
            ],
            writable_paths=[pathlib.Path(writable_path)] if writable_path is not None else [],
            executable_paths=[executable_path],
        )
        if landlock_prefix is None:
            raise ValueError("controller Linux Landlock isolation unavailable")
        return _handlers_shim._extend_pinned_launcher_command(
            landlock_prefix, [executable, *controller_tail]
        )
    if sys.platform.startswith("linux") and not path_denial:
        raise ValueError("controller Linux transport lacks holdout denial")
    executable = prepared[codex_index] if path_denial else "codex"
    return (outer if path_denial else []) + [executable, *controller_tail]



def _controller_codex_args(
    args: list[str],
    *,
    read_only_path: pathlib.Path | str | None = None,
    writable_path: pathlib.Path | str | None = None,
    schema_path: pathlib.Path | str | None = None,
) -> list[str]:
    """Backward-compatible shim for the controller transport builder."""
    return _build_controller_codex_transport(
        args,
        read_only_path=read_only_path,
        writable_path=writable_path,
        schema_path=schema_path,
    )


def _run_gate_once(
    backend: str, prompt: str, expected_sha: str, timeout: int, ctx: "Context", name: str,
    *, gate_strict: bool = False, read_only_path: pathlib.Path | str | None = None,
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
            outcome="error",
            output="sandbox-exec unavailable",
            metadata={"slash_command": name, "verdict": "unknown",
                      "reviewer_backend": reviewer_backend, "sandbox": "unavailable",
                      "backend_missing": "true",
                      **prompt_meta},
        )

    controller_requested = str(ctx.state.get("_df_controller_review_json") or "").lower() in {"true", "1", "yes", "on"}
    if controller_requested and backend != "codex":
        return Result(
            outcome="error",
            output="controller review transport requires codex backend",
            metadata={
                "slash_command": name,
                "verdict": "unknown",
                "reviewer_backend": reviewer_backend,
                "head_sha_status": "missing",
                "backend_missing": "true",
                **prompt_meta,
            },
        )
    controller_json = backend == "codex" and controller_requested
    runtime = None
    if controller_json:
        try:
            runtime = _handlers_shim._create_controller_runtime()
            sub_args = _controller_codex_args(
                sub_args,
                read_only_path=read_only_path or ctx.workdir,
                writable_path=runtime.codex_home,
                schema_path=_handlers_shim._controller_output_schema(runtime.run_dir),
            )
        except (OSError, RuntimeError, ValueError) as exc:
            if runtime is not None:
                try:
                    _handlers_shim._cleanup_controller_runtime(runtime.run_dir)
                except Exception:  # noqa: BLE001, S110 - best-effort runtime cleanup
                    pass
            return Result(
                outcome="error",
                output=f"controller review runtime/argv setup failed: {exc}",
                metadata={
                    "slash_command": name,
                    "verdict": "unknown",
                    "reviewer_backend": reviewer_backend,
                    **prompt_meta,
                },
            )
        sub_env = runtime.env
    shadow_review = _start_shadow_gate_review(name, prompt, expected_sha, timeout, ctx)

    def _finalize(result: "Result") -> "Result":
        return _finish_shadow_gate_review(result, shadow_review, name, expected_sha, timeout, ctx)

    # agy enforces its own --print-timeout; give the outer wait a small buffer
    # so we read agy's timeout message rather than killing it first.
    run_timeout = timeout + 30 if backend == "agy" else timeout
    try:
        review_cwd = ctx.workdir
        if controller_json:
            request = ctx.state.get("_df_controller_review_request")
            if request is None:
                raise RuntimeError("controller review request is missing")
            review_cwd = pathlib.Path(
                str(json.loads(request.envelope_json)["target"]["workspace_path"])
            )
        proc = subprocess.run(
            sub_args, cwd=review_cwd, capture_output=True, text=True,
            input=prompt if controller_json else None,
            timeout=run_timeout, check=False, env=sub_env,
            pass_fds=getattr(sub_args, "pass_fds", ()),
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
        if runtime is not None:
            try:
                _handlers_shim._cleanup_controller_runtime(runtime.run_dir)
            except Exception:  # noqa: BLE001, S110 - best-effort runtime cleanup
                pass
        return _finalize(Result(
            outcome="failure",
            output=combined.strip() or f"gate {name} timed out after {run_timeout}s",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "timed_out": "true",
                      "reviewer_backend": reviewer_backend, **prompt_meta},
        ))
    except FileNotFoundError as exc:
        if runtime is not None:
            try:
                _handlers_shim._cleanup_controller_runtime(runtime.run_dir)
            except Exception:  # noqa: BLE001, S110 - best-effort runtime cleanup
                pass
        return _finalize(Result(
            outcome="error",
            output=f"gate {name} backend {reviewer_backend!r} not found: {exc}",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "reviewer_backend": reviewer_backend,
                      "backend_missing": "true", **prompt_meta},
        ))
    except Exception as exc:
        if runtime is not None:
            try:
                _handlers_shim._cleanup_controller_runtime(runtime.run_dir)
            except Exception:  # noqa: BLE001, S110 - best-effort runtime cleanup
                pass
        return _finalize(Result(
            outcome="error",
            output=f"gate {name} subprocess failed: {exc}",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "reviewer_backend": reviewer_backend,
                      **prompt_meta},
        ))
    finally:
        _handlers_shim._close_pinned_launcher_command(sub_args)
    command_receipts = ()
    transport_receipt = None
    review_output = proc.stdout
    transport_error = ""
    controller_review = None
    if controller_json and proc.returncode == 0:
        try:
            from .review_controller import (
                parse_tool_free_codex_jsonl,
                validate_review_response,
            )

            request = ctx.state.get("_df_controller_review_request")
            if request is None:
                raise RuntimeError("controller review request is missing")
            review_output, transport_receipt = parse_tool_free_codex_jsonl(
                proc.stdout, request=request
            )
            controller_review = validate_review_response(review_output, request)
            if transport_receipt.head_sha != expected_sha:
                raise ValueError("tool-free controller receipt head binding mismatch")
            command_receipts = ()
        except Exception as exc:
            transport_error = str(exc)
            review_output = proc.stdout
    if controller_json:
        if controller_review is not None and not transport_error and proc.returncode == 0:
            verdict = controller_review.verdict
            normalized = "success" if verdict == "pass" else "failure"
            observed_sha = transport_receipt.head_sha if transport_receipt else ""
            sha_ok = observed_sha == expected_sha
            outcome = normalized if sha_ok else "error"
        else:
            verdict = "unknown"
            normalized = "error"
            observed_sha = ""
            sha_ok = False
            outcome = "error"
    else:
        combined = review_output + "\n" + proc.stderr
        verdict, normalized = _handlers_shim._parse_verdict(combined, gate_strict=gate_strict)
        # SHA binding check comes BEFORE collapsing to pass/fail so a spoofed-pass
        # with the wrong SHA collapses to `error`, not `success`.
        sha_ok, observed_sha = _handlers_shim._verify_head_sha_echo(combined, expected_sha)
        if transport_error:
            outcome = "error"
        elif proc.returncode != 0 and (verdict == "unknown" or normalized == "success"):
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
    # Build the structured receipt for the real subprocess that just ran.
    # This is what closes the regex-fabrication ceiling — the gate now binds
    # the verdict to the captured execution, not to the reviewer's narrative.
    receipt = None
    if not controller_json:
        receipt = _build_reviewer_receipt(
            sub_args=sub_args, proc=proc, cwd=str(ctx.workdir),
            expected_sha=expected_sha, timeout=timeout,
        )
    metadata: dict[str, Any] = {
        "slash_command": name, "verdict": verdict,
        "returncode": str(proc.returncode),
        "expected_head_sha": expected_sha, "observed_head_sha": observed_sha,
        "head_sha_status": head_sha_status,
        "reviewer_backend": reviewer_backend,
        **prompt_meta,
    }
    if receipt is not None:
        # Pass-through: list-valued receipt list is consumed verbatim by the
        # structured gate (_check_structured_receipt) via _MDToCtxShim.
        metadata["_reviewer_receipts"] = [receipt]
    if controller_json:
        metadata["_controller_runtime_root"] = str(runtime.run_dir) if runtime is not None else ""
        metadata["_controller_command_receipts"] = [
            {
                "command": item.command,
                "exit_code": item.exit_code,
                "output_sha256": item.output_sha256,
            }
            for item in command_receipts
        ]
        if transport_receipt is not None:
            metadata["_controller_transport_receipt"] = {
                "transport": transport_receipt.transport,
                "prompt_sha256": transport_receipt.prompt_sha256,
                "envelope_sha256": transport_receipt.envelope_sha256,
                "response_sha256": transport_receipt.response_sha256,
                "head_sha": transport_receipt.head_sha,
                "tree_sha": transport_receipt.tree_sha,
                "evidence_manifest_sha256": transport_receipt.evidence_manifest_sha256,
            }
        if transport_error:
            metadata["review_transport_error"] = transport_error
        # The parallel controller lane persists this exact raw JSONL through
        # the shared controller artifact writer after contract adjustment.
        # Keep it transiently in metadata so the graph path cannot fall back
        # to a generic sidecar or reconstruct transport from parsed prose.
        metadata["_controller_transport_text"] = proc.stdout
        metadata["_controller_transport_argv"] = list(sub_args)
    # Codergen-sourced receipts (Task 2): the codergen producer stashes
    # parsed ``commands_run.md`` records into ``ctx.state`` under per-node
    # keys ``"<node>.structured_receipt"``. The reviewer gate runs in a
    # SEPARATE node from the codergen node, so the receipts are NOT under
    # this gate's own key — gather every ``*.structured_receipt`` list from
    # ``ctx.state`` and surface them under a parallel metadata key so the
    # structured gate (_check_structured_receipt via _MDToCtxShim) can honor
    # them at the same trust tier as engine-captured receipts. No new config;
    # absent codergen receipts => the key is unset and behavior is unchanged.
    #
    # Cross-node gathering is intentional and by design. The plan describes
    # "codergen lanes" (plural): more than one codergen node may run against
    # the same HEAD, each producing its own structured receipt. The gather
    # loop below intentionally OR-aggregates receipts from *every*
    # ``*.structured_receipt`` key, not just the one belonging to a specific
    # codergen node. This is safe because the SHA check above
    # (``head_sha`` matching ``expected_sha``) binds each receipt to the
    # graded *commit*, not to a specific codergen node: every receipt
    # aggregated here already carries (and was validated against) the HEAD
    # being graded. Therefore a passing receipt from ANY codergen lane that
    # ran on the same HEAD legitimately satisfies the structured gate — the
    # commit-level provenance is what matters, not which lane produced it.
    codergen_receipts: list = []
    state = getattr(ctx, "state", None)
    if isinstance(state, dict):
        for k, v in state.items():
            if isinstance(k, str) and k.endswith(".structured_receipt") and isinstance(v, list):
                codergen_receipts.extend(v)
    if codergen_receipts:
        metadata["_codergen_receipts"] = codergen_receipts
    return _finalize(Result(
        outcome=outcome,
        output=review_output,
        metadata=metadata,
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


def _build_reviewer_receipt(
    *,
    sub_args: list[str],
    proc: "subprocess.CompletedProcess | None",
    cwd: str,
    expected_sha: str,
    timeout: int,
    lane_id: str = "primary",
) -> dict | None:
    """Build a structured receipt record from a captured subprocess result.

    Returns ``None`` when the subprocess has not been executed yet (early
    timeout/missing-exe branches), so the caller can drop it cleanly from
    the gate Result metadata. Otherwise returns a dict with the canonical
    shape consumed by ``runner.handler_verdict._check_structured_receipt``.
    """
    if proc is None:
        return None
    try:
        rc = int(getattr(proc, "returncode", 1) or 0)
    except (TypeError, ValueError):
        rc = 1
    return {
        "command": list(sub_args),
        "cwd": cwd,
        "exit_code": rc,
        "head_sha": str(expected_sha or "").lower(),
        "lane_id": lane_id,
        "timeout": int(timeout),
    }


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
    bin_name = "claude" if name == "claude-sonnet" else name
    bin_path = shutil.which(bin_name)
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
    *,
    controller_review: bool = False,
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
        if controller_review and name != "codex":
            skipped.append(f"{name}(no_controller_transport)")
            continue
        if _handlers_shim._probe_backend_installed(name):
            resolved = name
            break
        skipped.append(name)

    # Fall through to codex for controller review or last entry if uninstalled
    if resolved is None:
        resolved = "codex" if controller_review else (priority[-1] if priority else _DEFAULT_ADVERSARIAL_PRIORITY[-1])

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
            # Read-back tolerates both legacy dict and JSON string (for backward
            # compatibility during rollout). Malformed values fall back to {}.
            raw_meta = ctx.state.get(f"{node.name}.resolved_backend_meta")
            if isinstance(raw_meta, str):
                try:
                    prior_meta = json.loads(raw_meta)
                except (json.JSONDecodeError, TypeError):
                    prior_meta = {}
            elif isinstance(raw_meta, dict):
                prior_meta = raw_meta
            else:
                prior_meta = {}
            if prior and prior_meta.get("reviewer_backend_resolution") == "priority_queue":
                return prior, prior_meta
            # When prefer_adversarial is set, exclude the run-level coder
            # backend from the priority list (so a `claude` run with an
            # `agy` coder cannot accidentally get a `claude` reviewer).
            # ``prefer_adversarial`` is an ORDERING preference, not a hard
            # filter. Demoting the coder's own backend to last still yields a
            # different vendor whenever any other entry is installed, but when
            # it is the only one available the lane reviews on it rather than
            # erroring or escaping to an expensive vendor nobody asked for.
            # It used to DROP ``ctx.backend`` outright, which had two costs:
            # a same-coder-and-lane graph emptied the list (see below), and a
            # cheap-but-same vendor was skipped in favour of whatever came
            # next in the queue — frequently the priciest entry.
            if prefer_adversarial and ctx.backend and ctx.backend in priority:
                priority = [p for p in priority if p != ctx.backend] + [ctx.backend]
            # Reached only by a graph that names no usable entry at all (an
            # empty or whitespace-only ``backend_priority``). The demotion
            # above is order-preserving and never removes an entry, so it
            # cannot empty the list. A lane whose entries are all uninstalled
            # deliberately resolves to its own last entry -- ``_execute_gate``
            # then treats the missing binary as an infra failure and runs its
            # agy -> claude fallback. Do not add a second safety net here: it
            # would override a controller-review lane's codex-only queue and
            # break its fail-closed guarantee.
            if not priority:
                priority = list(_DEFAULT_ADVERSARIAL_PRIORITY)
            controller_review = bool(node.attrs.get("review_contract")) or str(ctx.state.get("_df_controller_review_json") or "").lower() in {"true", "1", "yes", "on"}
            resolved, pq_meta = _resolve_adversarial_backend(priority, ctx, controller_review=controller_review)

            ctx.state[prior_key] = resolved
            pq_meta["prefer_adversarial"] = "true" if prefer_adversarial else "false"
            pq_meta["reviewer_backend_resolution"] = "priority_queue"
            # Store as JSON string to satisfy the contract that ctx.state values
            # are strings (required by handler_render._substitute_placeholders).
            ctx.state[f"{node.name}.resolved_backend_meta"] = json.dumps(pq_meta, sort_keys=True)
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
    *, gate_strict: bool = False, read_only_path: pathlib.Path | str | None = None,
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
    result = _run_gate_once(
        backend,
        prompt,
        expected_sha,
        timeout,
        ctx,
        name,
        gate_strict=gate_strict,
        read_only_path=read_only_path,
    )

    controller_requested = str(ctx.state.get("_df_controller_review_json") or "").lower() in {"true", "1", "yes", "on"}
    if _is_gate_infra_failure(result):
        if controller_requested:
            result.metadata["verdict"] = "infra_failure"
            result.metadata.setdefault("fallback_used", "false")
            return result
        fallback_backends = []
        if backend not in ("agy", "claude", "claude-sonnet"):
            fallback_backends.append("agy")
        if backend not in ("claude", "claude-sonnet"):
            fallback_backends.append("claude")

        current_result = result
        for fb_backend in fallback_backends:
            fallback = _run_gate_once(
                fb_backend,
                prompt,
                expected_sha,
                timeout,
                ctx,
                name,
                gate_strict=gate_strict,
                read_only_path=read_only_path,
            )
            fallback.metadata["fallback_used"] = "true"
            fallback.metadata["fallback_from"] = backend
            current_result = fallback
            if not _is_gate_infra_failure(current_result):
                return current_result

        current_result.metadata["verdict"] = "infra_failure"
        return current_result

    result.metadata.setdefault("fallback_used", "false")
    return result
