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
import shutil
import subprocess
from typing import TYPE_CHECKING, Optional

import runner.handlers as _handlers_shim

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _gate_subprocess_args(backend: str, prompt: str, ctx: "Context", timeout: int) -> Optional[list[str]]:
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


def _run_gate_once(
    backend: str, prompt: str, expected_sha: str, timeout: int, ctx: "Context", name: str
) -> "Result":
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
    sub_args = _gate_subprocess_args(backend, prompt, ctx, timeout)
    sub_env = _gate_subprocess_env(backend)
    if sub_args is None:
        return Result(
            outcome="failure",
            output="sandbox-exec unavailable",
            metadata={"slash_command": name, "verdict": "unknown",
                      "reviewer_backend": reviewer_backend, "sandbox": "unavailable"},
        )
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
        return Result(
            outcome="failure",
            output=combined.strip() or f"gate {name} timed out after {run_timeout}s",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "timed_out": "true",
                      "reviewer_backend": reviewer_backend},
        )
    except FileNotFoundError as exc:
        return Result(
            outcome="error",
            output=f"gate {name} backend {reviewer_backend!r} not found: {exc}",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "reviewer_backend": reviewer_backend,
                      "backend_missing": "true"},
        )
    except Exception as exc:
        return Result(
            outcome="error",
            output=f"gate {name} subprocess failed: {exc}",
            metadata={"slash_command": name, "verdict": "unknown",
                      "head_sha_status": "missing", "reviewer_backend": reviewer_backend},
        )
    combined = proc.stdout + "\n" + proc.stderr
    verdict, normalized = _handlers_shim._parse_verdict(combined)
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
    return Result(
        outcome=outcome,
        output=proc.stdout,
        metadata={
            "slash_command": name, "verdict": verdict,
            "returncode": str(proc.returncode),
            "expected_head_sha": expected_sha, "observed_head_sha": observed_sha,
            "head_sha_status": head_sha_status,
            "reviewer_backend": reviewer_backend,
        },
    )


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
    if priority is None:
        raw = os.environ.get("DARK_FACTORY_ADVERSARIAL_PRIORITY", "")
        priority = _parse_priority_env(raw) if raw else list(_DEFAULT_ADVERSARIAL_PRIORITY)
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
    prompt: str, expected_sha: str, timeout: int, ctx: "Context", name: str, backend: str
) -> "Result":
    """Run a reviewer gate on ``backend``; infra failures fall back to claude.

    Routing rules:
      - Run the resolved backend. If the result is an *infrastructure*
        failure (missing binary, sandbox unavailable, timeout, unparseable
        output, SHA mismatch with no real verdict) and the backend is not
        already claude-routed, fall back to ``claude`` — recorded in
        metadata (``fallback_used`` / ``fallback_from``), never silent.
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
    result = _run_gate_once(backend, prompt, expected_sha, timeout, ctx, name)
    # minimax shares the claude CLI binary but grades via a different
    # gateway/model, so claude is still a genuine infra fallback for it.
    claude_routed = backend in ("claude", "claude-sonnet")
    if _is_gate_infra_failure(result) and not claude_routed:
        fallback = _run_gate_once("claude", prompt, expected_sha, timeout, ctx, name)
        fallback.metadata["fallback_used"] = "true"
        fallback.metadata["fallback_from"] = backend
        if _is_gate_infra_failure(fallback):
            fallback.metadata["verdict"] = "infra_failure"
        return fallback
    result.metadata.setdefault("fallback_used", "false")
    if _is_gate_infra_failure(result):
        result.metadata["verdict"] = "infra_failure"
    return result
