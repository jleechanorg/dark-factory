"""The single ``_codergen`` function.

Backends:
  - echo: no LLM — just record the rendered prompt. Used in tests.
  - mock_llm: POST to a local mock LLM server.
  - claude: shell out to ``claude --print`` with ``--dangerously-skip-permissions``.
  - codex: shell out to ``codex exec --yolo``.
  - agy: shell out to ``agy --print --dangerously-skip-permissions``.
  - ao: dispatch to an Agent Orchestrator worker. First call spawns a session
    (``ao spawn``); subsequent calls reuse it (``ao send``). The worker writes
    inside its own AO-managed worktree; the path is stored in
    ``ctx.state["ao.worktree"]`` so downstream tool nodes can target it.

Stays as a single function because splitting the 5 backend branches across
files would change the ``TYPE_REGISTRY`` contract. The runtime dispatch
lives in ``runner/handlers.py:resolve``.

All monkeypatched helper symbols (``_sanitized_env``, ``_sandboxed_args``,
``_sandboxed_args_for_workdir``, ``_get_claude_executable``, ``_ao_wait_idle``,
``_render_prompt``) are looked up via the ``runner.handlers`` shim (late
binding) so that ``monkeypatch.setattr("runner.handlers._X", ...)`` still
takes effect.

Sealed-path sandbox contract (jleechan-113)
-------------------------------------------

Implementing-agent coder subprocesses (claude, codex, agy, plus the parallel
codex shadow review) run inside ``ctx.workdir``. They use
``_sandboxed_args_for_workdir`` so the sandbox-exec deny rules cover BOTH
``$DARK_FACTORY_HOLDOUTS`` (the sealed sibling repo) AND the operator-only
sealed docs at ``<ctx.workdir>/benchmarks/*/{README,DESIGN,SCORING,SCENARIOS}.md``.
This prevents the implementing agent from reading held-back design notes or
scoring rubrics while coding.

The AO ``spawn`` / ``send`` calls deliberately continue to use the legacy
``_sandboxed_args`` because the AO orchestrator's own subprocess does not
read benchmark docs from ``ctx.workdir`` — the AO worker writes inside its
own AO-managed worktree, and the sandbox there is enforced at a different
layer (see ``tests/test_ao_sandbox.py`` for the AO-specific test coverage).

Per-backend timeout defaults
----------------------------

The roadmap at ``docs/plans/factory_improvement_analysis.md`` section
"Dynamic LLM Timeouts & Provider Backoff" proposes a codergen default of
**180 seconds**. This module uses **1800s** for claude/codex and **600s**
for agy — both deliberately exceed the roadmap value.

Rationale (jleechan-arr): production codergen wall-clock evidence from
``~/Library/Logs/dark-factory`` across real claude/codex/agy runs shows:

  * claude codergen p50 ≈ 276s, p90 ≈ 585s, p99 = 1800s (timeout hits)
  * codex codergen p99 = 1800s (timeout hits)

The 180s roadmap value would timeout roughly **50 % of observed claude
runs** at p50 alone. The 1800s default matches the observed p99 ceiling
without dropping too many runs; the 600s agy default matches the observed
agy distribution (agy is faster than claude in our samples). See
``TIMEOUT_DEFAULTS_RATIONALE`` in
``docs/plans/factory_improvement_analysis.implementation.md`` Pillar 4 for
the full empirical distribution and the citation.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context

# G4 diff-injection cap. Hard limit on the diff we paste into reviewer
# prompts to avoid blowing past LLM context windows on large PRs. The
# note appended on truncation surfaces the original byte count so the
# reviewer knows the diff was lossy (it can re-fetch via git if it
# needs the rest).
_DIFF_MAX_CHARS = 50_000

# commands_run.md (#406 artifact) exit-code line. Matches the forms the
# implementing agent writes — ``exit code: N`` — plus the ``exit_code: N``
# and ``exit: N`` variants, in the spirit of ``_RECEIPT_EXIT_RE`` in
# handler_verdict.py. Parsing is mechanical line-format parsing only.
_CMD_RUN_EXIT_RE = re.compile(
    r"(?:exit[_ ]?code\s*[:=]\s*|exit\s*[:=]\s*)(\d+)\b",
    re.IGNORECASE,
)


def _fresh_reviewer_check_gap(text: str) -> str:
    """Return a gap when an attempted reviewer check only failed.

    A fully-tooled reviewer is allowed to choose the repository's relevant
    checks.  If it does choose a test/build runner, however, a process-level
    PASS cannot hide an unavailable runtime (for example ``pytest`` returning
    126 or failing to import).  A successful check in the same transcript
    satisfies the requirement; otherwise the reviewer result fails closed.
    Narrative-only PASSes remain compatible with the existing slim contract.
    """
    body = text or ""
    from .handler_verdict import _RECEIPT_EXIT_RE, _RECEIPT_RUNNER_RE

    if not _RECEIPT_RUNNER_RE.search(body):
        return ""
    exit_codes = [int(match.group(1)) for match in _RECEIPT_EXIT_RE.finditer(body)]
    exit_codes.extend(
        int(match.group(1))
        for match in re.finditer(
            r"(?:\brc|\breturncode|\bstatus)\s*[:=]\s*(\d+)\b",
            body,
            re.IGNORECASE,
        )
    )
    if 0 in exit_codes:
        return ""
    runtime_failure = re.search(
        r"(?:no module named|module not found|command not found|not found|"
        r"permission denied|operation not permitted|cannot execute|"
        r"failed to (?:run|start)|could not (?:run|start))",
        body,
        re.IGNORECASE,
    )
    if not exit_codes and runtime_failure is None:
        return ""
    detail = (
        f"captured nonzero exit codes {sorted(set(exit_codes))}"
        if exit_codes
        else "runtime error without a successful exit code"
    )
    return (
        "required reviewer check failed; the reviewer returned PASS without a "
        f"successful test/build reproduction ({detail}). Restore the reviewer "
        "runtime or rerun the check successfully before accepting PASS."
    )


def _subprocess_output(stdout: str | bytes | None, stderr: str | bytes | None) -> str:
    """Combine subprocess output, including byte payloads from timeouts."""
    if isinstance(stdout, bytes):
        stdout = stdout.decode("utf-8", errors="replace")
    if isinstance(stderr, bytes):
        stderr = stderr.decode("utf-8", errors="replace")
    stdout = stdout or ""
    stderr = stderr or ""
    return (stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip()


@dataclass
class _ShadowCodexReview:
    prompt: str = ""
    proc: subprocess.Popen | None = None
    prompt_path: str = ""
    prompt_sha256: str = ""
    launch_error: str = ""
    started_at: float = 0.0


def _shadow_review_enabled(node: "Node", ctx: "Context", backend: str) -> bool:
    """True when a review node should get a parallel plain-Codex check."""
    verdict_gate = str(node.attrs.get("verdict_gate", "false")).strip().lower() in {
        "true", "1", "yes", "on",
    }
    if verdict_gate:
        return False
    raw = node.attrs.get("shadow_codex_review", ctx.state.get("_df_shadow_codex_review", "false"))
    if isinstance(raw, str) and raw.strip().lower() in {"false", "0", "no", "off"}:
        return False
    if raw is False:
        return False
    if backend in {"echo", "mock_llm"}:
        return False
    return str(node.attrs.get("class", "")).strip().lower() == "review"


def _shadow_review_prompt(node: "Node", ctx: "Context", primary_prompt: str) -> str:
    """Build the simple independent reviewer prompt for the Codex shadow lane."""
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


def _start_shadow_codex_review(
    node: "Node",
    ctx: "Context",
    backend: str,
    primary_prompt: str,
) -> _ShadowCodexReview | None:
    """Launch the plain Codex shadow review before the primary reviewer blocks."""
    if not _shadow_review_enabled(node, ctx, backend):
        return None

    prompt = _shadow_review_prompt(node, ctx, primary_prompt)
    shadow = _ShadowCodexReview(prompt=prompt, started_at=time.monotonic())
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

    if shutil.which("codex") is None:
        shadow.launch_error = "codex executable not found"
        return shadow

    args = _handlers_shim._sandboxed_args_for_workdir([
        "codex",
        "exec",
        "--yolo",
        "--skip-git-repo-check",
        prompt,
    ], ctx.workdir)
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
            env=_handlers_shim._sanitized_env(),
        )
    except Exception as exc:
        shadow.launch_error = f"{type(exc).__name__}: {exc}"
    return shadow


def _finish_shadow_codex_review(
    result: "Result",
    shadow: _ShadowCodexReview | None,
    node: "Node",
    ctx: "Context",
) -> "Result":
    """Merge the parallel Codex review into a reviewer node's result."""
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
            if proc.returncode != 0 or timed_out:
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
    return Result(
        outcome=final_outcome,
        output=result.output + comparison,
        metadata=meta,
        preferred_label=result.preferred_label,
        suggested_next_ids=result.suggested_next_ids,
        context_updates=updates,
    )


def _capture_diff(workdir: "Path | str | None") -> str:
    """Best-effort ``git diff`` capture for reviewer prompts (G4).

    Returns ``git diff`` + ``git diff --staged`` concatenated with a blank
    line. Returns an empty string on any failure (no workdir, git missing,
    not a repo, subprocess errors). The caller decides what to do with the
    empty result — by convention it is stashed verbatim into
    ``ctx.state["_last_diff"]`` so subsequent codergen calls see no diff
    yet and the renderer's default ``(no diff captured)`` placeholder is
    what the reviewer reads.

    The workdir may be either a Path (ctx.workdir) or a string
    (ctx.state["ao.worktree"]). For the AO backend, the implementing
    agent writes inside its AO-managed worktree — ``ctx.workdir`` is
    the runner cwd, not the coder's tree, so the caller must pass the
    AO worktree string when present.

    Defense in depth: ``ctx.state["ao.worktree"]`` is set by the AO
    backend itself, but a forged state value could point ``git -C`` at
    any filesystem location. Reject anything that isn't an absolute,
    non-traversing path to an existing directory. Best-effort: a bad
    path returns ``""`` so the renderer's ``(no diff captured)``
    placeholder is what the reviewer reads, never a partial diff
    from the wrong repo.
    """
    if not workdir:
        return ""
    wd_path = pathlib.Path(str(workdir))
    if not wd_path.is_absolute() or ".." in wd_path.parts or not wd_path.is_dir():
        return ""
    wd = str(wd_path)
    try:
        unstaged = subprocess.run(
            ["git", "-C", wd, "diff"],
            capture_output=True, text=True, timeout=15, check=False,
        )
        staged = subprocess.run(
            ["git", "-C", wd, "diff", "--staged"],
            capture_output=True, text=True, timeout=15, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    parts = []
    if unstaged.returncode == 0 and unstaged.stdout:
        parts.append(unstaged.stdout)
    if staged.returncode == 0 and staged.stdout:
        parts.append(staged.stdout)
    if not parts:
        return ""
    raw = "\n".join(parts)
    if len(raw) <= _DIFF_MAX_CHARS:
        return raw
    truncated = raw[:_DIFF_MAX_CHARS]
    note = f"\n... (truncated, full diff is {len(raw)} bytes)"
    return truncated + note


def _capture_changed_files(workdir: "Path | str | None") -> str:
    """Best-effort list of changed files for G5.

    Returns a bulleted list of changed files relative to workdir.
    """
    if not workdir:
        return ""
    wd_path = pathlib.Path(str(workdir))
    if not wd_path.is_absolute() or ".." in wd_path.parts or not wd_path.is_dir():
        return ""
    wd = str(wd_path)
    try:
        unstaged = subprocess.run(
            ["git", "-C", wd, "diff", "--name-only"],
            capture_output=True, text=True, timeout=15, check=False,
        )
        staged = subprocess.run(
            ["git", "-C", wd, "diff", "--staged", "--name-only"],
            capture_output=True, text=True, timeout=15, check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""
    files = set()
    if unstaged.returncode == 0 and unstaged.stdout:
        for line in unstaged.stdout.splitlines():
            line = line.strip()
            if line:
                files.add(line)
    if staged.returncode == 0 and staged.stdout:
        for line in staged.stdout.splitlines():
            line = line.strip()
            if line:
                files.add(line)
    if not files:
        return "(none)"
    return "\n".join(f"- {f}" for f in sorted(files))


def _stash_diff(node: "Node", ctx: "Context") -> None:
    """Stash the captured diff and changed files into ``ctx.state`` for reviewer prompts.

    Writes both ``ctx.state["<node.name>.diff"]`` and ``ctx.state["_last_diff"]``,
    as well as ``ctx.state["<node.name>.changed_files"]`` and ``ctx.state["_last_changed_files"]``.
    """
    workdir = _codergen_workdir(ctx)
    diff = _capture_diff(workdir)
    ctx.state[f"{node.name}.diff"] = diff
    ctx.state["_last_diff"] = diff

    changed_files = _capture_changed_files(workdir)
    ctx.state[f"{node.name}.changed_files"] = changed_files
    ctx.state["_last_changed_files"] = changed_files


def _codergen_workdir(ctx: "Context") -> "Path | str | None":
    """Resolve the worktree the coder ran in.

    Prefers ``ctx.state["ao.worktree"]`` when it is an absolute, non-traversing
    path to an existing directory; otherwise falls back to ``ctx.workdir``.
    Delegates to canonical ``_target_worktree`` helper.
    """
    try:
        return str(_handlers_shim._target_worktree(ctx))
    except Exception:
        try:
            return ctx.workdir
        except AttributeError:
            return None


@dataclass(frozen=True)
class _TrackedFingerprint:
    head: str | None = None
    unstaged_diff: str | None = None
    staged_diff: str | None = None
    untracked: str | None = None
    git_error: str = ""
    digest: str | None = None


def _compute_tracked_fingerprint(
    workdir: "Path | str | None", *, include_untracked: bool = False
) -> _TrackedFingerprint:
    """Compute component-level git fingerprint with diagnostic attribution."""
    if not workdir:
        return _TrackedFingerprint(git_error="empty workdir")
    path = pathlib.Path(str(workdir))
    if not path.is_absolute() or ".." in path.parts or not path.is_dir():
        return _TrackedFingerprint(git_error=f"invalid workdir path: {workdir}")

    head_val: str | None = None
    unstaged_val: str | None = None
    staged_val: str | None = None
    untracked_val: str | None = None

    commands = (
        ("head", ("rev-parse", "HEAD")),
        ("unstaged", ("diff", "--binary")),
        ("staged", ("diff", "--cached", "--binary")),
    )
    for name, tail in commands:
        try:
            proc = subprocess.run(
                ["git", "-C", str(path), *tail],
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
        except subprocess.TimeoutExpired:
            return _TrackedFingerprint(git_error=f"git {tail[0]} timed out")
        except OSError as exc:
            return _TrackedFingerprint(git_error=f"git {tail[0]} OSError: {exc}")
        if proc.returncode != 0:
            err = proc.stderr.strip() or proc.stdout.strip() or f"exit code {proc.returncode}"
            return _TrackedFingerprint(git_error=f"git {tail[0]} failed: {err}")
        if name == "head":
            head_val = proc.stdout
        elif name == "unstaged":
            unstaged_val = proc.stdout
        elif name == "staged":
            staged_val = proc.stdout

    outputs = [head_val or "", unstaged_val or "", staged_val or ""]

    if include_untracked:
        try:
            proc = subprocess.run(
                ["git", "-C", str(path), "ls-files", "--others", "-z"],
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
        except subprocess.TimeoutExpired:
            return _TrackedFingerprint(git_error="git ls-files timed out")
        except OSError as exc:
            return _TrackedFingerprint(git_error=f"git ls-files OSError: {exc}")
        if proc.returncode != 0:
            err = proc.stderr.strip() or proc.stdout.strip() or f"exit code {proc.returncode}"
            return _TrackedFingerprint(git_error=f"git ls-files failed: {err}")

        raw_files = [p for p in (proc.stdout or "").split("\0") if p]
        untracked_entries: list[str] = []
        for rel_str in sorted(raw_files):
            try:
                file_path = path / rel_str
                if file_path.is_symlink():
                    target = os.readlink(file_path)
                    entry_hash = hashlib.sha256(f"symlink:{target}".encode("utf-8")).hexdigest()
                elif file_path.is_file():
                    entry_hash = hashlib.sha256(file_path.read_bytes()).hexdigest()
                elif file_path.is_dir():
                    entry_hash = "dir"
                else:
                    entry_hash = "missing"
                untracked_entries.append(f"{rel_str}:{entry_hash}")
            except OSError as exc:
                return _TrackedFingerprint(git_error=f"cannot read untracked entry {rel_str}: {exc}")
        untracked_val = "\0".join(untracked_entries)
        outputs.append(untracked_val)

    digest = hashlib.sha256("\0".join(outputs).encode("utf-8")).hexdigest()
    return _TrackedFingerprint(
        head=head_val,
        unstaged_diff=unstaged_val,
        staged_diff=staged_val,
        untracked=untracked_val,
        git_error="",
        digest=digest,
    )


def _tracked_state(
    workdir: "Path | str | None", *, include_untracked: bool = False
) -> str | None:
    """Hash HEAD plus staged/unstaged changes, optionally including untracked artifacts."""
    return _compute_tracked_fingerprint(workdir, include_untracked=include_untracked).digest



def _fresh_review_workdir(ctx: "Context") -> pathlib.Path | None:
    """Return a real, non-symlinked target directory for a fresh reviewer."""
    raw = _codergen_workdir(ctx)
    if not raw:
        return None
    candidate = pathlib.Path(str(raw))
    if not candidate.is_absolute() or ".." in candidate.parts:
        return None
    try:
        resolved = candidate.resolve(strict=True)
    except (OSError, RuntimeError):
        return None
    if candidate.is_symlink() or not resolved.is_dir():
        return None
    return resolved


def _git_ignored_snapshot_paths(target_workdir: pathlib.Path) -> set[pathlib.Path]:
    """Return ignored, untracked paths that must not enter a review snapshot."""
    try:
        proc = subprocess.run(
            [
                "git",
                "-C",
                str(target_workdir),
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "--no-empty-directory",
                "-z",
            ],
            capture_output=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise RuntimeError("Cannot determine Git-ignored review paths") from exc
    if proc.returncode != 0:
        raise RuntimeError("Cannot determine Git-ignored review paths")
    return {
        pathlib.Path(os.fsdecode(raw).rstrip("/"))
        for raw in proc.stdout.split(b"\0")
        if raw
    }


def _validate_snapshot_symlinks(
    target_workdir: pathlib.Path, ignored_paths: set[pathlib.Path]
) -> None:
    """Validate that target_workdir contains no unsafe, escaping, or dangling symlinks."""
    target_real = target_workdir.resolve(strict=True)
    for root, dirs, files in os.walk(target_workdir, topdown=True, followlinks=False):
        root_path = pathlib.Path(root)
        root_relative = root_path.relative_to(target_workdir)
        dirs[:] = [
            name for name in dirs if root_relative / name not in ignored_paths
        ]
        for name in dirs + files:
            entry = root_path / name
            if root_relative / name in ignored_paths:
                continue
            if entry.is_symlink():
                try:
                    resolved = entry.resolve(strict=True)
                except (OSError, RuntimeError) as exc:
                    raise RuntimeError(
                        f"Target workspace contains unsafe dangling or looping symlink: {entry}"
                    ) from exc
                if not (resolved == target_real or target_real in resolved.parents):
                    raise RuntimeError(
                        f"Target workspace contains unsafe escaping symlink: {entry} -> {resolved}"
                    )
                entry_parent = entry.parent.resolve(strict=True)
                if resolved == entry_parent or resolved in entry_parent.parents:
                    raise RuntimeError(
                        f"Target workspace symlink resolves to its own ancestor or root: {entry}"
                    )
                resolved_relative = resolved.relative_to(target_real)
                if any(
                    ignored == resolved_relative
                    or ignored in resolved_relative.parents
                    or resolved_relative in ignored.parents
                    for ignored in ignored_paths
                ):
                    raise RuntimeError(
                        f"Target workspace symlink resolves into a Git-ignored path: {entry}"
                    )


def _materialize_snapshot_symlinks(target_workdir: pathlib.Path, review_dir: pathlib.Path) -> None:
    """Preserve safe relative links and materialize links that retain source reachability."""
    target_real = target_workdir.resolve(strict=True)
    review_real = review_dir.resolve(strict=True)
    for root, dirs, files in os.walk(review_dir, topdown=False, followlinks=False):
        root_path = pathlib.Path(root)
        for name in dirs + files:
            entry = root_path / name
            if entry.is_symlink():
                rel = entry.relative_to(review_dir)
                orig = target_workdir / rel
                try:
                    target_resolved = orig.resolve(strict=True)
                except (OSError, RuntimeError) as exc:
                    raise RuntimeError(f"Cannot resolve in-tree symlink {entry}") from exc
                if not (target_resolved == target_real or target_real in target_resolved.parents):
                    raise RuntimeError(f"Escaping symlink found during materialization: {entry}")
                if not pathlib.Path(os.readlink(orig)).is_absolute():
                    try:
                        review_resolved = entry.resolve(strict=True)
                    except (OSError, RuntimeError) as exc:
                        raise RuntimeError(f"Cannot resolve copied symlink {entry}") from exc
                    if review_real in review_resolved.parents:
                        continue
                entry.unlink()
                if target_resolved.is_dir():
                    shutil.copytree(target_resolved, entry, symlinks=False)
                else:
                    shutil.copy2(target_resolved, entry)


def _normalize_and_validate_review_git(target_workdir: pathlib.Path, review_dir: pathlib.Path) -> None:
    """Normalize linked-worktree git metadata into a standalone repo and validate isolation."""
    git_target = target_workdir / ".git"
    git_review = review_dir / ".git"
    gitdir_path: pathlib.Path | None = None
    main_git_dir: pathlib.Path | None = None

    if git_target.is_file():
        gitdir_content = git_target.read_text(encoding="utf-8").strip()
        if not gitdir_content.startswith("gitdir:"):
            raise RuntimeError(f"Invalid worktree .git pointer file in {target_workdir}: {gitdir_content}")
        gitdir_raw = gitdir_content[len("gitdir:"):].strip()
        gitdir_path = (target_workdir / gitdir_raw).resolve()
        if not gitdir_path.is_dir():
            raise RuntimeError(f"Worktree gitdir does not exist or is not a directory: {gitdir_path}")

        commondir_file = gitdir_path / "commondir"
        if commondir_file.is_file():
            commondir_raw = commondir_file.read_text(encoding="utf-8").strip()
            main_git_dir = (gitdir_path / commondir_raw).resolve()
        else:
            main_git_dir = gitdir_path
        if not main_git_dir.is_dir():
            raise RuntimeError(f"Common gitdir does not exist or is not a directory: {main_git_dir}")

        if git_review.exists() or git_review.is_symlink():
            if git_review.is_dir():
                shutil.rmtree(git_review)
            else:
                git_review.unlink()

        shutil.copytree(
            main_git_dir,
            git_review,
            symlinks=True,
            ignore_dangling_symlinks=False,
        )

        for entry in gitdir_path.rglob("*"):
            if entry.is_dir() or entry.name in ("commondir", "gitdir"):
                continue
            rel = entry.relative_to(gitdir_path)
            dest = git_review / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            if dest.exists() or dest.is_symlink():
                if dest.is_dir():
                    shutil.rmtree(dest)
                else:
                    dest.unlink()
            shutil.copy2(entry, dest)

    if git_review.exists():
        if not git_review.is_dir() or git_review.is_symlink():
            raise RuntimeError(f"Review worktree .git is not a real directory: {git_review}")

        wt_sub = git_review / "worktrees"
        if wt_sub.exists():
            if wt_sub.is_dir():
                shutil.rmtree(wt_sub)
            else:
                wt_sub.unlink()

        for name in ("commondir", "gitdir"):
            p = git_review / name
            if p.exists():
                p.unlink()

        cfg_file = git_review / "config"
        if cfg_file.is_file():
            content = cfg_file.read_text(encoding="utf-8")
            new_content = re.sub(r"bare\s*=\s*true", "bare = false", content, flags=re.IGNORECASE)
            new_lines = [
                line for line in new_content.splitlines()
                if not line.strip().lower().startswith("worktree =")
                and not line.strip().lower().startswith("worktreerepo =")
            ]
            cfg_file.write_text("\n".join(new_lines) + "\n", encoding="utf-8")

        # Validate that no metadata file in git_review references original repository paths
        forbidden_paths: list[bytes] = [str(target_workdir).encode("utf-8")]
        if gitdir_path is not None:
            forbidden_paths.append(str(gitdir_path).encode("utf-8"))
        if main_git_dir is not None:
            forbidden_paths.append(str(main_git_dir).encode("utf-8"))

        for f in git_review.rglob("*"):
            if f.is_symlink():
                try:
                    sym_target = f.resolve(strict=True)
                except Exception as exc:
                    raise RuntimeError(f"Unsafe symlink in review git metadata: {f}") from exc
                if not (sym_target == git_review or git_review in sym_target.parents):
                    raise RuntimeError(f"Escaping symlink in review git metadata: {f} -> {sym_target}")
            elif f.is_file():
                content = f.read_bytes()
                for forbidden in forbidden_paths:
                    if forbidden in content:
                        raise RuntimeError(
                            f"Review git metadata references original repository path: {f}"
                        )


@contextlib.contextmanager
def _isolated_review_workdir(target_workdir: pathlib.Path):
    """Create an isolated, temporary workspace for fresh reviewer execution.

    Preserves tracked files, staged/unstaged changes, unignored untracked
    artifacts, and git repository state so the reviewer can run tools and
    inspect worker output, while containing any reviewer writes away from the
    actual coder target worktree.
    """
    ignored_paths = _git_ignored_snapshot_paths(target_workdir)
    _validate_snapshot_symlinks(target_workdir, ignored_paths)
    with tempfile.TemporaryDirectory(prefix="df-fresh-review-") as tmpdir:
        tmp_path = pathlib.Path(tmpdir)
        review_dir = tmp_path / "workspace"

        def ignore_git_ignored(directory: str, names: list[str]) -> list[str]:
            relative = pathlib.Path(directory).relative_to(target_workdir)
            return [name for name in names if relative / name in ignored_paths]

        shutil.copytree(
            target_workdir,
            review_dir,
            symlinks=True,
            ignore_dangling_symlinks=False,
            ignore=ignore_git_ignored,
        )
        _materialize_snapshot_symlinks(target_workdir, review_dir)
        _normalize_and_validate_review_git(target_workdir, review_dir)
        yield review_dir


def _linux_python_reviewer_runtime_paths() -> list[pathlib.Path] | None:
    """Resolve trusted Python and pytest runtimes for Linux review tools.

    Landlock starts with an allow-list.  The Codex launcher paths alone are
    insufficient for a reviewer command such as ``pytest``: its executable,
    interpreter, and virtual-environment roots must also be readable.  Reuse
    the existing trusted executable resolver so a broken or untrusted PATH
    entry fails closed instead of making a reviewer report a false PASS.
    """
    candidates: list[pathlib.Path] = []
    pytest_path = shutil.which("pytest")
    if pytest_path:
        candidates.append(pathlib.Path(pytest_path))
    if sys.executable:
        candidates.append(pathlib.Path(sys.executable))

    def runtime_root(executable: pathlib.Path) -> pathlib.Path | None:
        """Return the exact Python/venv prefix containing ``bin`` and ``lib``."""
        try:
            root = executable.resolve(strict=True).parent.parent
        except (OSError, RuntimeError):
            return None
        if (
            root == pathlib.Path(root.anchor)
            or not (root / "bin").is_dir()
            or not (root / "lib").is_dir()
        ):
            return None
        return root

    def shebang_interpreter(executable: pathlib.Path) -> pathlib.Path | None:
        try:
            first_line = executable.resolve(strict=True).read_text(
                encoding="utf-8", errors="replace"
            ).splitlines()[0]
        except (OSError, IndexError, RuntimeError):
            return None
        if not first_line.startswith("#!"):
            return None
        interpreter = first_line[2:].strip().split(maxsplit=1)[0]
        path = pathlib.Path(interpreter)
        return path if path.is_absolute() else None

    paths: list[pathlib.Path] = []
    resolved_candidates: list[pathlib.Path] = []
    for candidate in candidates:
        if not candidate.exists() and not candidate.is_symlink():
            # ``shutil.which`` may be replaced by a test harness or may point
            # at a stale optional test runner.  Do not make Codex itself
            # unavailable because pytest is absent; if the reviewer elects
            # that check, ``_fresh_reviewer_check_gap`` fails the PASS closed.
            continue
        runtime = _handlers_shim._linux_codex_runtime_paths(candidate)
        if runtime is None:
            return None
        paths.extend(runtime)
        resolved_candidates.append(candidate)
        interpreter = shebang_interpreter(candidate)
        if interpreter is not None:
            interpreter_runtime = _handlers_shim._linux_codex_runtime_paths(interpreter)
            if interpreter_runtime is None:
                return None
            paths.extend(interpreter_runtime)
            resolved_candidates.append(interpreter)

    for executable in resolved_candidates:
        root = runtime_root(executable)
        if root is not None:
            paths.append(root)
    return sorted(set(paths), key=str)


def _sandboxed_args_for_fresh_review(
    command: list[str],
    codex_workdir: pathlib.Path,
    target_workdir: pathlib.Path,
    *,
    runtime_home: pathlib.Path | None = None,
) -> list[str] | None:
    """Build sandboxed arguments for fresh review isolating against target_workdir writes.

    On macOS: Seatbelt sandbox-exec profile with holdouts denied and target_workdir denied.
    On Linux: Landlock allow-list allowing only codex_workdir, private codex runtime home,
    and codex runtime paths, and denying target_workdir and holdouts.
    Fails closed if Landlock kernel boundary is unavailable.
    """
    if os.environ.get("DISABLE_SANDBOX"):
        raise ValueError(
            "fresh review refuses DISABLE_SANDBOX; isolation is required"
        )
    from . import handler_sandbox as _sandbox

    shim_fn = getattr(_handlers_shim, "_sandboxed_args_for_workdir", None)
    if shim_fn is not None and shim_fn is not _sandbox._sandboxed_args_for_workdir:
        return shim_fn(command, codex_workdir)

    if sys.platform == "darwin":
        if not _handlers_shim._verify_darwin_sandbox_exec():
            return None
        sandbox_exec = shutil.which("sandbox-exec")
        if sandbox_exec is None:
            return None
        extra_denied = [target_workdir.resolve()]
        profile = _sandbox._build_sandbox_profile(extra_denied)
        return [sandbox_exec, "-p", profile] + command

    if sys.platform.startswith("linux"):
        launcher = _handlers_shim._linux_landlock_launcher_path()
        if launcher is None:
            return None
        abi = _handlers_shim._linux_landlock_abi()
        if abi is None or abi < 3:
            return None
        denied_paths = _handlers_shim._holdout_denied_paths() + [target_workdir.resolve()]

        executable = command[0]
        resolved_executable = shutil.which(executable) or executable
        launcher_path = pathlib.Path(resolved_executable)
        runtime_paths = _handlers_shim._linux_codex_runtime_paths(launcher_path)
        if runtime_paths is None:
            return None
        python_runtime_paths = _linux_python_reviewer_runtime_paths()
        if python_runtime_paths is None:
            return None
        real_binary = _sandbox._linux_codex_real_binary(launcher_path)
        if real_binary is not None:
            executable_path = real_binary
            cmd_args = [str(real_binary), *command[1:]]
        else:
            executable_path = launcher_path
            cmd_args = command

        read_paths = [
            *runtime_paths,
            *python_runtime_paths,
            codex_workdir.resolve(),
        ]
        writable_paths = [codex_workdir.resolve()]
        if runtime_home is not None:
            read_paths.append(runtime_home.resolve())
            writable_paths.append(runtime_home.resolve())

        landlock_prefix = _handlers_shim._linux_controller_sandbox_prefix(
            denied_paths=denied_paths,
            read_paths=read_paths,
            writable_paths=writable_paths,
            executable_paths=[executable_path],
        )
        if landlock_prefix is None:
            return None
        return _handlers_shim._extend_pinned_launcher_command(
            landlock_prefix, cmd_args
        )
    return None



def _parse_commands_run_md(text: str) -> list[tuple[str, int]]:
    """Mechanical line-format parser for the ``commands_run.md`` #406 artifact.

    The artifact is lines of ``$ <command>`` each followed by an
    ``exit code: N`` line (``exit_code: N`` / ``exit: N`` also accepted).
    Returns a list of ``(command, exit_code)`` pairs. Malformed lines — an
    exit-code line with no preceding command, a command with no following
    exit code, or an unparseable exit integer — are skipped without raising.
    """
    out: list[tuple[str, int]] = []
    pending: str | None = None
    for raw in (text or "").splitlines():
        line = raw.rstrip()
        if line.startswith("$ "):
            pending = line[2:]
            continue
        m = _CMD_RUN_EXIT_RE.search(line)
        if not m:
            continue
        if pending is None:
            continue
        try:
            ec = int(m.group(1))
        except ValueError:
            continue
        out.append((pending, ec))
        pending = None
    return out


def _stash_codergen_receipt(node: "Node", ctx: "Context") -> None:
    """Parse ``commands_run.md`` from the coder worktree into structured receipts.

    On a SUCCESS outcome, if the worktree contains ``commands_run.md``, parse it
    into the structured receipt record shape consumed by
    ``runner.handler_verdict._check_structured_receipt`` and attach the list to
    ``ctx.state["<node.name>.structured_receipt"]``. Absent file or no parsed
    records => the key is left unset (existing behavior unchanged). All I/O is
    best-effort; this never raises.
    """
    workdir = _codergen_workdir(ctx)
    if not workdir:
        return
    commands_path = pathlib.Path(str(workdir)) / "commands_run.md"
    if not commands_path.is_file():
        return
    try:
        text = commands_path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return
    parsed = _parse_commands_run_md(text)
    if not parsed:
        return
    head_sha = ""
    try:
        sha = _handlers_shim._worktree_head_sha(pathlib.Path(str(workdir)))
        if sha:
            head_sha = sha
    except Exception:
        pass
    cwd = str(workdir)
    records = [
        {
            "command": [cmd],
            "cwd": cwd,
            "exit_code": ec,
            "head_sha": head_sha,
            "lane_id": node.name,
        }
        for cmd, ec in parsed
    ]
    ctx.state[f"{node.name}.structured_receipt"] = records


def _codergen(node: "Node", ctx: "Context") -> "Result":
    """Run an LLM coding step.

    Reads the prompt template referenced by `prompt="@path"` (relative to the
    runner workdir), substitutes `${goal}` and `${state.<key>}` placeholders,
    and dispatches to the configured backend.
    """
    prompt_text = _handlers_shim._render_prompt(node, ctx)
    backend = node.attrs.get("backend", node.attrs.get("model", ctx.backend))
    if isinstance(backend, bool):
        backend = ctx.backend
    backend = str(backend)
    verdict_gate = str(node.attrs.get("verdict_gate", "false")).strip().lower() in {
        "true", "1", "yes", "on",
    }
    if backend == "codex" and verdict_gate and os.environ.get("DISABLE_SANDBOX"):
        return Result(
            outcome="error",
            output="verdict-gated fresh review refuses DISABLE_SANDBOX; isolation is required",
            metadata={"verdict": "unknown", "fresh_session": "true"},
        )
    _start_ts = time.monotonic()
    shadow_review = _start_shadow_codex_review(node, ctx, backend, prompt_text)

    def _finalize(result: "Result") -> "Result":
        return _finish_shadow_codex_review(result, shadow_review, node, ctx)

    if backend == "echo":
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        metrics = _handlers_shim._codergen_metrics("", "", wall_ms)
        # Stringify so the metadata dict matches Result's declared type (str
        # values) and round-trips cleanly through the CXDB JSON column.
        meta = {k: ("" if v is None else str(v)) for k, v in metrics.items()}
        # Allow tests to drive branch outcomes via ctx.state["<node>.outcome"]
        # (same convention as human_gate pre-seeding).
        pre = ctx.state.get(f"{node.name}.outcome")
        outcome = pre if pre is not None else "success"
        if outcome == "success":
            _stash_diff(node, ctx)
            _stash_codergen_receipt(node, ctx)
        return _finalize(Result(outcome=outcome, output=prompt_text, metadata=meta))

    if backend == "mock_llm":
        mock_url = str(ctx.state.get("mock_url", "")).rstrip("/")
        endpoint = f"{mock_url}/responses" if "/responses" not in mock_url else mock_url
        import urllib.request
        payload = json.dumps({"model": "gpt-4o", "input": prompt_text}).encode("utf-8")
        req = urllib.request.Request(
            endpoint,
            data=payload,
            headers={
                "Content-Type": "application/json",
                "Authorization": "Bearer test-key"
            },
            method="POST"
        )
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                resp_data = json.loads(resp.read().decode("utf-8"))
        except Exception as e:
            return _finalize(Result(outcome="failure", output=f"mock LLM error: {e}"))

        output_parts = resp_data.get("output", [])
        content_text = ""
        if output_parts and isinstance(output_parts, list):
            part = output_parts[0]
            if "content" in part and isinstance(part["content"], list):
                content_text = part["content"][0].get("text", "")
            elif "content" in part and isinstance(part["content"], str):
                content_text = part["content"]
        if not content_text:
            choices = resp_data.get("choices", [])
            if choices and isinstance(choices, list):
                msg = choices[0].get("message", {})
                content_text = msg.get("content", "")
            else:
                content_text = json.dumps(resp_data)

        usage = resp_data.get("usage", {})
        input_tokens = usage.get("input_tokens", 0)
        output_tokens = usage.get("output_tokens", 0)
        total_tokens = usage.get("total_tokens", 0)
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        meta = {
            "api_calls": "1",
            "input_tokens": str(input_tokens),
            "output_tokens": str(output_tokens),
            "total_tokens": str(total_tokens),
            "wall_ms": str(wall_ms),
        }
        _stash_diff(node, ctx)
        _stash_codergen_receipt(node, ctx)
        return _finalize(Result(outcome="success", output=content_text, metadata=meta))

    if backend == "ao":
        project = ctx.state.get("ao.project")
        if not project:
            return _finalize(Result(outcome="failure", output="ao backend requires --ao-project"))
        agent = ctx.state.get("ao.agent", "claude-code")
        session = ctx.state.get("ao.session")
        if not session:
            spawn_args = ["ao", "spawn", prompt_text, "--project", project, "--agent", agent]
            spawn_args = _handlers_shim._sandboxed_args(spawn_args)
            if spawn_args is None:
                return _finalize(Result(outcome="failure", output="sandbox-exec unavailable"))
            ao_spawn_timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "300"), 300)
            try:
                proc = subprocess.run(
                    spawn_args,
                    cwd=ctx.workdir,
                    capture_output=True,
                    text=True,
                    timeout=ao_spawn_timeout,
                    check=False,
                    env=_handlers_shim._sanitized_env(),
                )
            except subprocess.TimeoutExpired as exc:
                return _finalize(Result(
                    outcome="failure",
                    output=_subprocess_output(exc.stdout, exc.stderr)
                    or f"ao spawn timed out after {ao_spawn_timeout} seconds",
                    metadata={
                        "session": "",
                        "activity": "timeout",
                        "timed_out": "true",
                        "timeout": str(ao_spawn_timeout),
                        "returncode": "",
                    },
                ))
            except Exception as exc:
                return _finalize(Result(
                    outcome="failure",
                    output=f"ao spawn failed: {exc}",
                    metadata={
                        "session": "",
                        "activity": "error",
                        "timed_out": "false",
                        "timeout": str(ao_spawn_timeout),
                        "returncode": "",
                    },
                ))
            if proc.returncode != 0:
                return _finalize(Result(
                    outcome="failure",
                    output=f"ao spawn failed (rc={proc.returncode})\n{proc.stdout}\nSTDERR:\n{proc.stderr}",
                    metadata={
                        "session": "",
                        "returncode": str(proc.returncode),
                        "timed_out": "false",
                        "timeout": str(ao_spawn_timeout),
                        "activity": "spawn_failed",
                    },
                ))
            sess_name = None
            worktree = None
            for line in proc.stdout.splitlines():
                if line.startswith("SESSION="):
                    sess_name = line.split("=", 1)[1].strip()
                m = re.search(r"Worktree:\s*(\S+)", line)
                if m:
                    worktree = m.group(1)
            if not sess_name:
                return _finalize(Result(outcome="failure", output=f"ao spawn produced no SESSION= line\n{proc.stdout}"))
            ctx.state["ao.session"] = sess_name
            if worktree:
                ctx.state["ao.worktree"] = worktree
            ao_wait_timeout = _handlers_shim._coerce_timeout(node.attrs.get("wait_timeout", "900"), 900)
            activity = _handlers_shim._ao_wait_idle(sess_name, ctx.workdir, timeout=ao_wait_timeout, project=project)
            outcome = "success" if activity in ("exited", "ready") else "failure"
            wall_ms = int((time.monotonic() - _start_ts) * 1000)
            metrics = _handlers_shim._codergen_metrics(proc.stdout, proc.stderr, wall_ms)
            meta = {
                "session": sess_name,
                "worktree": worktree or "",
                "activity": activity,
                "timed_out": "true" if activity == "timeout" else "false",
                "timeout": str(ao_wait_timeout),
                "returncode": str(proc.returncode),
            }
            meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
            if outcome == "success":
                _stash_diff(node, ctx)
                _stash_codergen_receipt(node, ctx)
            return _finalize(Result(
                outcome=outcome,
                output=f"ao spawn session={sess_name} worktree={worktree} activity={activity}",
                metadata=meta,
            ))

        ao_send_timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "960"), 960)
        send_args = _handlers_shim._sandboxed_args([
            "ao",
            "send",
            session,
            prompt_text,
            "--timeout",
            str(ao_send_timeout),
        ])
        if send_args is None:
            return _finalize(Result(outcome="failure", output="sandbox-exec unavailable"))
        try:
            proc = subprocess.run(
                send_args,
                cwd=ctx.workdir,
                capture_output=True,
                text=True,
                timeout=ao_send_timeout + 120,
                check=False,
                env=_handlers_shim._sanitized_env(),
            )
        except subprocess.TimeoutExpired as exc:
            return _finalize(Result(
                outcome="failure",
                output=_subprocess_output(exc.stdout, exc.stderr)
                or f"ao send timed out after {ao_send_timeout} seconds",
                metadata={
                    "session": session,
                    "activity": "timeout",
                    "timed_out": "true",
                    "timeout": str(ao_send_timeout),
                    "returncode": "",
                },
            ))
        except Exception as exc:
            return _finalize(Result(
                outcome="failure",
                output=f"ao send failed: {exc}",
                metadata={
                    "session": session,
                    "activity": "error",
                    "timed_out": "false",
                    "timeout": str(ao_send_timeout),
                    "returncode": "",
                },
            ))
        if proc.returncode != 0:
            if "does not exist" in proc.stdout or "does not exist" in proc.stderr:
                if "ao.session" in ctx.state:
                    del ctx.state["ao.session"]
                if "ao.worktree" in ctx.state:
                    del ctx.state["ao.worktree"]
            return _finalize(Result(
                outcome="failure",
                output=f"ao send failed (rc={proc.returncode})\n{proc.stdout}\nSTDERR:\n{proc.stderr}",
                metadata={
                    "session": session,
                    "activity": "send_failed",
                    "timed_out": "false",
                    "timeout": str(ao_send_timeout),
                    "returncode": str(proc.returncode),
                },
            ))
        ao_wait_timeout = _handlers_shim._coerce_timeout(node.attrs.get("wait_timeout", "900"), 900)
        activity = _handlers_shim._ao_wait_idle(session, ctx.workdir, timeout=ao_wait_timeout, project=project)
        outcome = "success" if activity in ("exited", "ready") else "failure"
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        metrics = _handlers_shim._codergen_metrics(proc.stdout, proc.stderr, wall_ms)
        meta = {
            "session": session,
            "activity": activity,
            "timed_out": "true" if activity == "timeout" else "false",
            "timeout": str(ao_wait_timeout),
            "returncode": str(proc.returncode),
        }
        meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
        if outcome == "success":
            _stash_diff(node, ctx)
            _stash_codergen_receipt(node, ctx)
        return _finalize(Result(
            outcome=outcome,
            output=f"ao send session={session} activity={activity}",
            metadata=meta,
        ))

    if backend == "claude":
        # `--output-format json` makes coder token usage + dollar cost observable
        # (the cost axis is blind under plain `--print`). The envelope is parsed
        # by `_claude_json_result`; `output` is still the readable result text.
        claude_cmd = [_handlers_shim._get_claude_executable(), "--print", "--output-format", "json",
                      "--dangerously-skip-permissions", "--setting-sources", ""]
        # `model_name` (not `model`) pins the coder model via --model. `model` is
        # deliberately NOT read here: line ~246 already treats a bare `model`
        # attr as a backend alias, so reusing it would misroute a node that sets
        # only `model` to a nonexistent backend named after the model string.
        model_name = node.attrs.get("model_name")
        if model_name:
            claude_cmd += ["--model", str(model_name)]
        claude_cmd.append(prompt_text)
        args = _handlers_shim._sandboxed_args_for_workdir(claude_cmd, ctx.workdir)
        if args is None:
            return _finalize(Result(outcome="failure", output="sandbox-exec unavailable"))
        try:
            timeout_s = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "1800"), 1800)
            proc = subprocess.run(
                args,
                cwd=ctx.workdir,
                capture_output=True,
                text=True,
                timeout=timeout_s,
                check=False,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
                env=_handlers_shim._sanitized_env(),
            )
        except subprocess.TimeoutExpired:
            return _finalize(Result(
                outcome="failure",
                output=f"claude backend timed out after {timeout_s} seconds",
                metadata={
                    "timed_out": "true",
                    "timeout": str(timeout_s),
                    "returncode": "",
                },
            ))
        except Exception as e:
            return _finalize(Result(
                outcome="failure",
                output=f"claude backend error: {e}",
                metadata={
                    "timed_out": "false",
                    "timeout": str(timeout_s),
                    "returncode": "",
                },
            ))
        # Success path: parse the JSON envelope for output text + token/cost
        # metrics, then return directly (codex/agy keep the regex-based tail).
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        output_text, metrics = _handlers_shim._claude_json_result(proc.stdout, proc.stderr, wall_ms)
        outcome = "success" if proc.returncode == 0 else "failure"
        output = output_text + ("\nSTDERR:\n" + proc.stderr if proc.stderr else "")
        meta = {"returncode": str(proc.returncode)}
        meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
        if outcome == "success":
            _stash_diff(node, ctx)
            _stash_codergen_receipt(node, ctx)
        return _finalize(Result(outcome=outcome, output=output, metadata=meta))
    elif backend == "codex":
        target_workdir = _fresh_review_workdir(ctx) if verdict_gate else ctx.workdir
        if target_workdir is None:
            return _finalize(Result(
                outcome="error",
                output="fresh reviewer target must be a real, non-symlinked directory",
                metadata={"verdict": "unknown", "fresh_session": "true"},
            ))
        target_before_fp = _compute_tracked_fingerprint(target_workdir, include_untracked=True) if verdict_gate else None
        target_tracked_before = target_before_fp.digest if target_before_fp is not None else None
        if verdict_gate and target_tracked_before is None:
            err_msg = target_before_fp.git_error if target_before_fp is not None else ""
            return _finalize(Result(
                outcome="error",
                output="fresh reviewer could not fingerprint the tracked repository state",
                metadata={
                    "verdict": "unknown",
                    "fresh_session": "true",
                    "fingerprint_git_error": err_msg,
                },
            ))
        workdir_cm = (
            _isolated_review_workdir(target_workdir)
            if verdict_gate
            else contextlib.nullcontext(target_workdir)
        )
        try:
            with workdir_cm as codex_workdir:
                snapshot_before_fp = _compute_tracked_fingerprint(codex_workdir) if verdict_gate else None
                snapshot_tracked_before = snapshot_before_fp.digest if snapshot_before_fp is not None else None
                codex_command = ["codex", "exec"]
                if str(node.attrs.get("fresh_session", "false")).strip().lower() in {
                    "true", "1", "yes", "on",
                }:
                    codex_command.append("--ephemeral")
                codex_command.extend(["--yolo", "--skip-git-repo-check", prompt_text])

                runtime = None
                env = _handlers_shim._sanitized_env()
                if (
                    verdict_gate
                    and sys.platform.startswith("linux")
                ):
                    try:
                        runtime = _handlers_shim._create_controller_runtime()
                        env = runtime.env
                    except Exception as exc:
                        return _finalize(Result(
                            outcome="error",
                            output=f"failed to initialize private codex runtime: {exc}",
                            metadata={"verdict": "unknown", "fresh_session": "true"},
                        ))

                args = (
                    _sandboxed_args_for_fresh_review(
                        codex_command,
                        codex_workdir,
                        target_workdir,
                        runtime_home=runtime.codex_home if runtime is not None else None,
                    )
                    if verdict_gate
                    else _handlers_shim._sandboxed_args_for_workdir(
                        codex_command,
                        codex_workdir,
                    )
                )
                if args is None:
                    if runtime is not None:
                        try:
                            _handlers_shim._cleanup_controller_runtime(runtime.run_dir)
                        except Exception:
                            pass
                    return _finalize(Result(
                        outcome="error" if verdict_gate else "failure",
                        output="sandbox-exec unavailable" if sys.platform == "darwin" else "isolation unavailable",
                        metadata={"verdict": "unknown", "fresh_session": "true"} if verdict_gate else {},
                    ))
                timeout_s = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "1800"), 1800)
                try:
                    proc = subprocess.run(
                        args,
                        cwd=codex_workdir,
                        capture_output=True,
                        text=True,
                        timeout=timeout_s,
                        check=False,
                        input="",
                        env=env,
                        pass_fds=getattr(args, "pass_fds", ()),
                    )
                except subprocess.TimeoutExpired as exc:
                    return _finalize(Result(
                        outcome="error" if verdict_gate else "failure",
                        output=_subprocess_output(exc.stdout, exc.stderr)
                        or f"codex backend timed out after {timeout_s} seconds",
                        metadata={
                            "timed_out": "true",
                            "timeout": str(timeout_s),
                            "returncode": "",
                        },
                    ))
                except Exception as exc:
                    return _finalize(Result(
                        outcome="error",
                        output=f"codex backend error: {exc}",
                        metadata={
                            "timed_out": "false",
                            "timeout": str(timeout_s),
                            "returncode": "",
                        },
                    ))
                finally:
                    _handlers_shim._close_pinned_launcher_command(args)
                    if runtime is not None:
                        try:
                            _handlers_shim._cleanup_controller_runtime(runtime.run_dir)
                        except Exception:
                            pass

                output = proc.stdout + ("\nSTDERR:\n" + proc.stderr if proc.stderr else "")
                outcome = "success" if proc.returncode == 0 else "failure"
                wall_ms = int((time.monotonic() - _start_ts) * 1000)
                metrics = _handlers_shim._codergen_metrics(proc.stdout, proc.stderr, wall_ms)
                meta = {"returncode": str(proc.returncode)}
                meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
                if verdict_gate:
                    verdict, parsed_outcome = _handlers_shim._parse_verdict(output, gate_strict=True)
                    snapshot_after_fp = _compute_tracked_fingerprint(codex_workdir)
                    target_after_fp = _compute_tracked_fingerprint(target_workdir, include_untracked=True)
                    snapshot_tracked_after = snapshot_after_fp.digest
                    target_tracked_after = target_after_fp.digest
                    snapshot_mutated = (
                        snapshot_tracked_before is None
                        or snapshot_tracked_after is None
                        or snapshot_tracked_before != snapshot_tracked_after
                    )
                    target_mutated = (
                        target_tracked_before is None
                        or target_tracked_after is None
                        or target_tracked_before != target_tracked_after
                    )
                    mutated = snapshot_mutated or target_mutated

                    # Component-level attribution
                    head_changed = (
                        (snapshot_before_fp is not None and snapshot_after_fp is not None and snapshot_before_fp.head != snapshot_after_fp.head)
                        or (target_before_fp is not None and target_after_fp is not None and target_before_fp.head != target_after_fp.head)
                    )
                    unstaged_changed = (
                        (snapshot_before_fp is not None and snapshot_after_fp is not None and snapshot_before_fp.unstaged_diff != snapshot_after_fp.unstaged_diff)
                        or (target_before_fp is not None and target_after_fp is not None and target_before_fp.unstaged_diff != target_after_fp.unstaged_diff)
                    )
                    staged_changed = (
                        (snapshot_before_fp is not None and snapshot_after_fp is not None and snapshot_before_fp.staged_diff != snapshot_after_fp.staged_diff)
                        or (target_before_fp is not None and target_after_fp is not None and target_before_fp.staged_diff != target_after_fp.staged_diff)
                    )
                    untracked_changed = (
                        target_before_fp is not None and target_after_fp is not None and target_before_fp.untracked != target_after_fp.untracked
                    )
                    git_error = (
                        (snapshot_before_fp.git_error if snapshot_before_fp else "")
                        or (snapshot_after_fp.git_error if snapshot_after_fp else "")
                        or (target_before_fp.git_error if target_before_fp else "")
                        or (target_after_fp.git_error if target_after_fp else "")
                    )

                    if proc.returncode != 0 or verdict == "unknown":
                        outcome = "error"
                    else:
                        outcome = parsed_outcome
                    if outcome == "success":
                        check_gap = _fresh_reviewer_check_gap(output)
                        if check_gap:
                            outcome = "error"
                            meta.update(
                                {
                                    "reviewer_check_failed": "true",
                                    "reviewer_check_gap": check_gap,
                                }
                            )
                            output = output.rstrip() + "\n\n" + check_gap + "\n"
                    meta.update(
                        {
                            "verdict": verdict,
                            "fresh_session": "true",
                            "review_workdir": str(target_workdir),
                            "reviewer_mutated_tracked_files": str(mutated).lower(),
                            "fingerprint_head_changed": str(head_changed).lower(),
                            "fingerprint_unstaged_changed": str(unstaged_changed).lower(),
                            "fingerprint_staged_changed": str(staged_changed).lower(),
                            "fingerprint_untracked_changed": str(untracked_changed).lower(),
                            "fingerprint_git_error": git_error,
                        }
                    )
                    if mutated:
                        outcome = "error"
                        output = output.rstrip() + "\n\nReviewer changed tracked files; changes must be made by the coder.\n"
                elif outcome == "success":
                    _stash_diff(node, ctx)
                    _stash_codergen_receipt(node, ctx)
                return _finalize(Result(
                    outcome=outcome,
                    output=output,
                    metadata=meta,
                ))
        except Exception as exc:
            return _finalize(Result(
                outcome="error",
                output=f"failed to isolate review workspace: {exc}",
                metadata={"verdict": "unknown", "fresh_session": "true"},
            ))
    elif backend == "agy":
        timeout_s = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "600"), 600)
        agent_name = (
            node.attrs.get("agy_agent")
            or node.attrs.get("agent")
            or ctx.state.get("agy.agent")
            or ctx.state.get("agy_agent")
            or "gemini-3.6-flash-high"
        )
        task_dir = ctx.workdir / ".dark-factory"
        task_dir.mkdir(parents=True, exist_ok=True)
        task_file = task_dir / f"agy-task-{node.name}.md"
        agy_prompt = (
            "You are the implementation agent for a Dark Factory pipeline node.\n"
            "Run headlessly and non-interactively in the current working directory.\n"
            "For broad implementation work, decompose the task and use Antigravity "
            "subagents or parallel internal workers when the CLI makes that available; "
            "collapse their outputs into direct workspace edits before exiting.\n"
            "Make the requested file edits directly. "
            "Do not enter planning mode. Do not ask for approval. "
            "Do not wait for hooks, screenshots, or operator input. "
            "When finished, print a concise summary and stop.\n\n"
            f"{prompt_text}"
        )
        task_file.write_text(agy_prompt)
        launch_prompt = (
            f"Execute the Dark Factory task in {task_file}. "
            "Read that file, make the required workspace edits, run the relevant local checks, "
            "do not enter planning mode, do not ask for approval, "
            "print a concise completion summary, and stop."
        )
        args = _handlers_shim._sandboxed_args_for_workdir([
            "agy",
            "--agent",
            str(agent_name),
            "--add-dir",
            str(ctx.workdir),
            "--dangerously-skip-permissions",
            "--print-timeout",
            f"{timeout_s}s",
            "--print",
            launch_prompt,
        ], ctx.workdir)
        if args is None:
            return _finalize(Result(outcome="failure", output="sandbox-exec unavailable"))
        try:
            proc = subprocess.Popen(
                args,
                cwd=ctx.workdir,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
                env=_handlers_shim._sanitized_env(),
            )
        except FileNotFoundError as exc:
            return _finalize(Result(
                outcome="error",
                output=f"agy backend not found: {exc}",
                metadata={"returncode": "127", "backend_missing": "true", "agy_agent": str(agent_name)},
            ))
        try:
            stdout, stderr = proc.communicate(timeout=timeout_s + 30)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
                stdout, stderr = proc.communicate(timeout=5)
            except Exception:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except Exception:
                    pass
                stdout, stderr = proc.communicate()
            output = stdout + ("\nSTDERR:\n" + stderr if stderr else "")
            wall_ms = int((time.monotonic() - _start_ts) * 1000)
            metrics = _handlers_shim._codergen_metrics(stdout, stderr, wall_ms)
            meta = {"returncode": str(proc.returncode if proc.returncode is not None else ""), "timed_out": "true", "agy_agent": str(agent_name)}
            meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
            return _finalize(Result(
                outcome="failure",
                output=f"agy backend timed out after {timeout_s + 30}s\n{output}",
                metadata=meta,
            ))
        output = stdout + ("\nSTDERR:\n" + stderr if stderr else "")
        outcome = "success" if proc.returncode == 0 else "failure"
        if output.strip().startswith("Error: timed out waiting for response"):
            outcome = "failure"
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        metrics = _handlers_shim._codergen_metrics(stdout, stderr, wall_ms)
        meta = {"returncode": str(proc.returncode), "agy_agent": str(agent_name)}
        meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
        if outcome == "success":
            _stash_diff(node, ctx)
            _stash_codergen_receipt(node, ctx)
        return _finalize(Result(outcome=outcome, output=output, metadata=meta))
    else:
        return _finalize(Result(outcome="failure", output=f"unknown backend {backend!r}"))
