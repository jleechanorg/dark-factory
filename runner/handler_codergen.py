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
``_get_claude_executable``, ``_ao_wait_idle``, ``_render_prompt``) are looked
up via the ``runner.handlers`` shim (late binding) so that
``monkeypatch.setattr("runner.handlers._X", ...)`` still takes effect.

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

import json
import os
import pathlib
import re
import signal
import subprocess
import time
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


def _stash_diff(node: "Node", ctx: "Context") -> None:
    """Stash the captured diff into ``ctx.state`` for reviewer prompts.

    Writes both ``ctx.state["<node.name>.diff"]`` (per-node, scoped) and
    ``ctx.state["_last_diff"]`` (rolling, the most recent successful
    codergen diff — what ``${diff}`` substitutes against). Best-effort:
    if git fails or the workdir is not a repo, ``_last_diff`` becomes
    ``""`` (which the renderer turns into ``"(no diff captured)"``).
    """
    workdir: "Path | str | None" = None
    ao_wt = ctx.state.get("ao.worktree")
    # Defense in depth: a forged or stale ``ao.worktree`` value could
    # point ``git -C`` at an unintended repo. Validate before use;
    # fall back to ``ctx.workdir`` if the AO worktree is missing,
    # relative, traversing, or doesn't exist.
    ao_wt_valid = False
    if ao_wt:
        ao_path = pathlib.Path(str(ao_wt))
        if (
            ao_path.is_absolute()
            and ".." not in ao_path.parts
            and ao_path.is_dir()
        ):
            ao_wt_valid = True
    if ao_wt_valid:
        workdir = str(ao_wt)
    else:
        try:
            workdir = ctx.workdir
        except AttributeError:
            workdir = None
    diff = _capture_diff(workdir)
    ctx.state[f"{node.name}.diff"] = diff
    ctx.state["_last_diff"] = diff


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
    _start_ts = time.monotonic()
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
        return Result(outcome=outcome, output=prompt_text, metadata=meta)

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
            return Result(outcome="failure", output=f"mock LLM error: {e}")

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
        return Result(outcome="success", output=content_text, metadata=meta)

    if backend == "ao":
        project = ctx.state.get("ao.project")
        if not project:
            return Result(outcome="failure", output="ao backend requires --ao-project")
        agent = ctx.state.get("ao.agent", "claude-code")
        session = ctx.state.get("ao.session")
        if not session:
            spawn_args = ["ao", "spawn", prompt_text, "-p", project, "--agent", agent]
            spawn_args = _handlers_shim._sandboxed_args(spawn_args)
            if spawn_args is None:
                return Result(outcome="failure", output="sandbox-exec unavailable")
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
                stdout = exc.stdout or ""
                stderr = exc.stderr or ""
                return Result(
                    outcome="failure",
                    output=(stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip()
                    or f"ao spawn timed out after {ao_spawn_timeout} seconds",
                    metadata={
                        "session": "",
                        "activity": "timeout",
                        "timed_out": "true",
                        "timeout": str(ao_spawn_timeout),
                        "returncode": "",
                    },
                )
            except Exception as exc:
                return Result(
                    outcome="failure",
                    output=f"ao spawn failed: {exc}",
                    metadata={
                        "session": "",
                        "activity": "error",
                        "timed_out": "false",
                        "timeout": str(ao_spawn_timeout),
                        "returncode": "",
                    },
                )
            if proc.returncode != 0:
                return Result(
                    outcome="failure",
                    output=f"ao spawn failed (rc={proc.returncode})\n{proc.stdout}\nSTDERR:\n{proc.stderr}",
                    metadata={
                        "session": "",
                        "returncode": str(proc.returncode),
                        "timed_out": "false",
                        "timeout": str(ao_spawn_timeout),
                        "activity": "spawn_failed",
                    },
                )
            sess_name = None
            worktree = None
            for line in proc.stdout.splitlines():
                if line.startswith("SESSION="):
                    sess_name = line.split("=", 1)[1].strip()
                m = re.search(r"Worktree:\s*(\S+)", line)
                if m:
                    worktree = m.group(1)
            if not sess_name:
                return Result(outcome="failure", output=f"ao spawn produced no SESSION= line\n{proc.stdout}")
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
            return Result(
                outcome=outcome,
                output=f"ao spawn session={sess_name} worktree={worktree} activity={activity}",
                metadata=meta,
            )

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
            return Result(outcome="failure", output="sandbox-exec unavailable")
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
            stdout = exc.stdout or ""
            stderr = exc.stderr or ""
            return Result(
                outcome="failure",
                output=(stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip()
                or f"ao send timed out after {ao_send_timeout} seconds",
                metadata={
                    "session": session,
                    "activity": "timeout",
                    "timed_out": "true",
                    "timeout": str(ao_send_timeout),
                    "returncode": "",
                },
            )
        except Exception as exc:
            return Result(
                outcome="failure",
                output=f"ao send failed: {exc}",
                metadata={
                    "session": session,
                    "activity": "error",
                    "timed_out": "false",
                    "timeout": str(ao_send_timeout),
                    "returncode": "",
                },
            )
        if proc.returncode != 0:
            if "does not exist" in proc.stdout or "does not exist" in proc.stderr:
                if "ao.session" in ctx.state:
                    del ctx.state["ao.session"]
                if "ao.worktree" in ctx.state:
                    del ctx.state["ao.worktree"]
            return Result(
                outcome="failure",
                output=f"ao send failed (rc={proc.returncode})\n{proc.stdout}\nSTDERR:\n{proc.stderr}",
                metadata={
                    "session": session,
                    "activity": "send_failed",
                    "timed_out": "false",
                    "timeout": str(ao_send_timeout),
                    "returncode": str(proc.returncode),
                },
            )
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
        return Result(
            outcome=outcome,
            output=f"ao send session={session} activity={activity}",
            metadata=meta,
        )

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
        args = _handlers_shim._sandboxed_args(claude_cmd)
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
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
            return Result(
                outcome="failure",
                output=f"claude backend timed out after {timeout_s} seconds",
                metadata={
                    "timed_out": "true",
                    "timeout": str(timeout_s),
                    "returncode": "",
                },
            )
        except Exception as e:
            return Result(
                outcome="failure",
                output=f"claude backend error: {e}",
                metadata={
                    "timed_out": "false",
                    "timeout": str(timeout_s),
                    "returncode": "",
                },
            )
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
        return Result(outcome=outcome, output=output, metadata=meta)
    elif backend == "codex":
        args = _handlers_shim._sandboxed_args(["codex", "exec", "--yolo", "--skip-git-repo-check", prompt_text])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        timeout_s = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "1800"), 1800)
        try:
            proc = subprocess.run(
                args,
                cwd=ctx.workdir,
                capture_output=True,
                text=True,
                timeout=timeout_s,
                check=False,
                input="",
                env=_handlers_shim._sanitized_env(),
            )
        except subprocess.TimeoutExpired as exc:
            stdout = exc.stdout or ""
            stderr = exc.stderr or ""
            return Result(
                outcome="failure",
                output=(stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip()
                or f"codex backend timed out after {timeout_s} seconds",
                metadata={
                    "timed_out": "true",
                    "timeout": str(timeout_s),
                    "returncode": "",
                },
            )
        except Exception as exc:
            return Result(
                outcome="error",
                output=f"codex backend error: {exc}",
                metadata={
                    "timed_out": "false",
                    "timeout": str(timeout_s),
                    "returncode": "",
                },
            )
    elif backend == "agy":
        timeout_s = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "600"), 600)
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
        args = _handlers_shim._sandboxed_args([
            "agy",
            "--add-dir",
            str(ctx.workdir),
            "--dangerously-skip-permissions",
            "--print-timeout",
            f"{timeout_s}s",
            "--print",
            launch_prompt,
        ])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        proc = subprocess.Popen(
            args,
            cwd=ctx.workdir,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
            env=_handlers_shim._sanitized_env(),
        )
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
            meta = {"returncode": str(proc.returncode if proc.returncode is not None else ""), "timed_out": "true"}
            meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
            return Result(
                outcome="failure",
                output=f"agy backend timed out after {timeout_s + 30}s\n{output}",
                metadata=meta,
            )
        output = stdout + ("\nSTDERR:\n" + stderr if stderr else "")
        outcome = "success" if proc.returncode == 0 else "failure"
        if output.strip().startswith("Error: timed out waiting for response"):
            outcome = "failure"
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        metrics = _handlers_shim._codergen_metrics(stdout, stderr, wall_ms)
        meta = {"returncode": str(proc.returncode)}
        meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
        if outcome == "success":
            _stash_diff(node, ctx)
        return Result(outcome=outcome, output=output, metadata=meta)
    else:
        return Result(outcome="failure", output=f"unknown backend {backend!r}")

    output = proc.stdout + ("\nSTDERR:\n" + proc.stderr if proc.stderr else "")
    outcome = "success" if proc.returncode == 0 else "failure"
    if backend == "agy" and output.strip().startswith("Error: timed out waiting for response"):
        outcome = "failure"
    wall_ms = int((time.monotonic() - _start_ts) * 1000)
    metrics = _handlers_shim._codergen_metrics(proc.stdout, proc.stderr, wall_ms)
    meta = {"returncode": str(proc.returncode)}
    meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
    if outcome == "success":
        _stash_diff(node, ctx)
    return Result(
        outcome=outcome,
        output=output,
        metadata=meta,
    )
