"""Node handlers — what each node *does* when the engine visits it.

Handlers are looked up by node shape. Each handler returns a Result describing
the outcome, which the engine uses to pick the next edge.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Callable, Optional

from .parser import Node, is_start_node, is_exit_node
from .paths import factory_home

if TYPE_CHECKING:
    from .perf_log import GitContext, PerfRun


@dataclass
class Result:
    outcome: str = "success"  # used by edge `condition="outcome=success"`
    output: str = ""
    metadata: dict[str, str] = field(default_factory=dict)
    preferred_label: str = ""
    suggested_next_ids: list[str] = field(default_factory=list)
    context_updates: dict[str, str] = field(default_factory=dict)


@dataclass
class Context:
    """Mutable run state passed to every handler."""

    goal: str
    workdir: pathlib.Path
    state: dict[str, str] = field(default_factory=dict)
    history: list[dict[str, str]] = field(default_factory=list)
    backend: str = "echo"  # echo | mock_llm | ao | claude | codex | agy
    cxdb_path: Optional[pathlib.Path] = None
    run_id: Optional[str] = None
    event_log_path: Optional[pathlib.Path] = None
    perf_log_root: Optional[pathlib.Path] = None
    git_ctx: Optional["GitContext"] = None
    perf_run: Optional["PerfRun"] = None


_TIMEOUT_MIN_SECONDS = 5
_TIMEOUT_MAX_SECONDS = 3600


def _coerce_timeout(value: object, default: int, *, minimum: int = _TIMEOUT_MIN_SECONDS, maximum: int = _TIMEOUT_MAX_SECONDS) -> int:
    """Parse and clamp timeout values to the policy envelope.

    Invalid values fall back to `default`. Very small / very large values are
    clamped to prevent pathological zero-timeout runs or runaway hangs.
    """
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return default
    if parsed < minimum:
        return minimum
    if parsed > maximum:
        return maximum
    return parsed


Handler = Callable[[Node, Context], Result]


def _start(node: Node, ctx: Context) -> Result:
    return Result(outcome="success", output=f"start: {ctx.goal!r}")


def _exit(node: Node, ctx: Context) -> Result:
    unresolved = ctx.state.get("_unresolved_failure")
    if unresolved:
        return Result(outcome=unresolved, output=f"exit after unresolved {unresolved}")
    if ctx.history:
        previous = ctx.history[-1].get("outcome", "success")
        if previous != "success":
            return Result(outcome=previous, output=f"exit after {previous}")
    return Result(outcome="success", output="exit")


def _sanitized_env() -> dict[str, str]:
    env = {}
    for k, v in os.environ.items():
        if k == "DARK_FACTORY_HOLDOUTS":
            continue
        if "HOLDOUT" in k.upper():
            continue
        env[k] = v
    return env


def _get_claude_executable() -> str:
    # PATH wins so tests can intercept with a fake claude binary on PATH
    # (see tests/test_gates.py::test_gate_nonzero_returncode_cannot_spoof_pass).
    # If nothing on PATH, fall back to the user's nvm-installed binary as a
    # convenience so live runs don't depend on PATH being just-so.
    on_path = shutil.which("claude")
    if on_path:
        return on_path
    nvm_claude = pathlib.Path.home() / ".nvm" / "versions" / "node" / "v22.22.0" / "bin" / "claude"
    if nvm_claude.exists():
        return str(nvm_claude)
    return "claude"



def _holdouts_repo_path() -> pathlib.Path:
    repo = os.environ.get(
        "DARK_FACTORY_HOLDOUTS",
        str(pathlib.Path.home() / "projects" / "dark-factory-holdouts"),
    )
    return pathlib.Path(repo).expanduser().resolve()


def _holdout_denied_paths() -> list[pathlib.Path]:
    paths = {_holdouts_repo_path()}
    paths.add((pathlib.Path.home() / "projects" / "dark-factory-holdouts").resolve())
    return sorted(paths, key=lambda p: str(p))


def _sandboxed_args(args: list[str]) -> Optional[list[str]]:
    # Skip sandbox if DISABLE_SANDBOX env is set (for testing)
    if os.environ.get("DISABLE_SANDBOX"):
        return args
    sandbox_exec = shutil.which("sandbox-exec")
    if sandbox_exec is None:
        return None
    denies = []
    for path in _holdout_denied_paths():
        holdouts_repo = str(path).replace("\\", "\\\\").replace('"', '\\"')
        denies.append(f'(deny file-read* (subpath "{holdouts_repo}"))')
        denies.append(f'(deny file-write* (subpath "{holdouts_repo}"))')
    deny_rules = "\n".join(denies)
    profile = f"""
(version 1)
(allow default)
{deny_rules}
"""
    return [sandbox_exec, "-p", profile] + args


def _ao_parse_status(stdout: str, session: str) -> str:
    """Pull a session's `activity` from `ao status --json` output.

    `ao status` prepends notifier noise lines before the JSON array; strip
    everything before the first `[`.
    """
    idx = stdout.find("[")
    if idx < 0:
        return "unknown"
    try:
        data = json.loads(stdout[idx:])
    except json.JSONDecodeError:
        return "unknown"
    for entry in data:
        if entry.get("name") == session:
            return str(entry.get("activity", "unknown"))
    return "missing"


def _ao_wait_idle(
    session: str,
    workdir: pathlib.Path,
    timeout: int = 900,
    stable_reads: int = 3,
    poll_interval: int = 10,
    project: Optional[str] = None,
) -> str:
    """Poll `ao status --json` until the session is idle for `stable_reads`
    consecutive polls.

    During retry loops inside the agent (e.g. claude rate-limit backoff), a
    session can momentarily report "ready" between retry attempts before
    bouncing back to "active". Requiring N consecutive idle reads makes the
    wait robust against that.

    `project` filters the status query (`ao status -p <project> --json`),
    which is much faster than the unfiltered call when the fleet has many
    sessions.

    Returns the last observed terminal activity ("exited", "ready",
    "missing"), or "timeout" if the deadline elapsed before idle stabilised.
    """
    deadline = time.monotonic() + timeout
    consecutive = 0
    status_cmd = ["ao", "status", "--json"]
    if project:
        status_cmd = ["ao", "status", "-p", project, "--json"]
    while time.monotonic() < deadline:
        proc = subprocess.run(
            status_cmd,
            cwd=workdir,
            capture_output=True,
            text=True,
            timeout=180,
            check=False,
            env=_sanitized_env(),
        )
        if proc.returncode == 0:
            activity = _ao_parse_status(proc.stdout, session)
            if activity in ("exited", "missing"):
                return activity
            if activity == "ready":
                consecutive += 1
                last_terminal = "ready"
                if consecutive >= stable_reads:
                    return "ready"
            else:
                consecutive = 0
        time.sleep(poll_interval)
    return "timeout"


def _codergen(node: Node, ctx: Context) -> Result:
    """Run an LLM coding step.

    Reads the prompt template referenced by `prompt="@path"` (relative to the
    runner workdir), substitutes `${goal}` and `${state.<key>}` placeholders,
    and dispatches to the configured backend.

    Backends:
      - echo: no LLM — just record the rendered prompt. Used in tests.
      - claude: shell out to `claude --print` with --dangerously-skip-permissions
      - codex: shell out to `codex exec --yolo`
      - agy: shell out to `agy --print --dangerously-skip-permissions`
      - ao: dispatch to an Agent Orchestrator worker. First call spawns a
        session (`ao spawn`); subsequent calls reuse it (`ao send`). The
        worker writes inside its own AO-managed worktree; the path is stored
        in `ctx.state["ao.worktree"]` so downstream tool nodes can target it.
    """
    prompt_text = _render_prompt(node, ctx)
    backend = node.attrs.get("backend", node.attrs.get("model", ctx.backend))
    if isinstance(backend, bool):
        backend = ctx.backend
    backend = str(backend)
    _start_ts = time.monotonic()
    if backend == "echo":
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        metrics = _codergen_metrics("", "", wall_ms)
        # Stringify so the metadata dict matches Result's declared type (str
        # values) and round-trips cleanly through the CXDB JSON column.
        meta = {k: ("" if v is None else str(v)) for k, v in metrics.items()}
        # Allow tests to drive branch outcomes via ctx.state["<node>.outcome"]
        # (same convention as human_gate pre-seeding).
        pre = ctx.state.get(f"{node.name}.outcome")
        outcome = pre if pre is not None else "success"
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
        return Result(outcome="success", output=content_text, metadata=meta)

    if backend == "ao":
        project = ctx.state.get("ao.project")
        if not project:
            return Result(outcome="failure", output="ao backend requires --ao-project")
        agent = ctx.state.get("ao.agent", "claude-code")
        session = ctx.state.get("ao.session")
        if not session:
            spawn_args = ["ao", "spawn", prompt_text, "-p", project, "--agent", agent]
            spawn_args = _sandboxed_args(spawn_args)
            if spawn_args is None:
                return Result(outcome="failure", output="sandbox-exec unavailable")
            ao_spawn_timeout = _coerce_timeout(node.attrs.get("timeout", "300"), 300)
            try:
                proc = subprocess.run(
                    spawn_args,
                    cwd=ctx.workdir,
                    capture_output=True,
                    text=True,
                    timeout=ao_spawn_timeout,
                    check=False,
                    env=_sanitized_env(),
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
            ao_wait_timeout = _coerce_timeout(node.attrs.get("wait_timeout", "900"), 900)
            activity = _ao_wait_idle(sess_name, ctx.workdir, timeout=ao_wait_timeout, project=project)
            outcome = "success" if activity in ("exited", "ready") else "failure"
            wall_ms = int((time.monotonic() - _start_ts) * 1000)
            metrics = _codergen_metrics(proc.stdout, proc.stderr, wall_ms)
            meta = {
                "session": sess_name,
                "worktree": worktree or "",
                "activity": activity,
                "timed_out": "true" if activity == "timeout" else "false",
                "timeout": str(ao_wait_timeout),
                "returncode": str(proc.returncode),
            }
            meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
            return Result(
                outcome=outcome,
                output=f"ao spawn session={sess_name} worktree={worktree} activity={activity}",
                metadata=meta,
            )

        ao_send_timeout = _coerce_timeout(node.attrs.get("timeout", "960"), 960)
        send_args = _sandboxed_args([
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
                env=_sanitized_env(),
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
        ao_wait_timeout = _coerce_timeout(node.attrs.get("wait_timeout", "900"), 900)
        activity = _ao_wait_idle(session, ctx.workdir, timeout=ao_wait_timeout, project=project)
        outcome = "success" if activity in ("exited", "ready") else "failure"
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        metrics = _codergen_metrics(proc.stdout, proc.stderr, wall_ms)
        meta = {
            "session": session,
            "activity": activity,
            "timed_out": "true" if activity == "timeout" else "false",
            "timeout": str(ao_wait_timeout),
            "returncode": str(proc.returncode),
        }
        meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
        return Result(
            outcome=outcome,
            output=f"ao send session={session} activity={activity}",
            metadata=meta,
        )

    if backend == "claude":
        # `--output-format json` makes coder token usage + dollar cost observable
        # (the cost axis is blind under plain `--print`). The envelope is parsed
        # by `_claude_json_result`; `output` is still the readable result text.
        claude_cmd = [_get_claude_executable(), "--print", "--output-format", "json",
                      "--dangerously-skip-permissions", "--setting-sources", ""]
        # `model_name` (not `model`) pins the coder model via --model. `model` is
        # deliberately NOT read here: line ~246 already treats a bare `model`
        # attr as a backend alias, so reusing it would misroute a node that sets
        # only `model` to a nonexistent backend named after the model string.
        model_name = node.attrs.get("model_name")
        if model_name:
            claude_cmd += ["--model", str(model_name)]
        claude_cmd.append(prompt_text)
        args = _sandboxed_args(claude_cmd)
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        try:
            timeout_s = _coerce_timeout(node.attrs.get("timeout", "1800"), 1800)
            proc = subprocess.run(
                args,
                cwd=ctx.workdir,
                capture_output=True,
                text=True,
                timeout=timeout_s,
                check=False,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
                env=_sanitized_env(),
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
        output_text, metrics = _claude_json_result(proc.stdout, proc.stderr, wall_ms)
        outcome = "success" if proc.returncode == 0 else "failure"
        output = output_text + ("\nSTDERR:\n" + proc.stderr if proc.stderr else "")
        meta = {"returncode": str(proc.returncode)}
        meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
        return Result(outcome=outcome, output=output, metadata=meta)
    elif backend == "codex":
        args = _sandboxed_args(["codex", "exec", "--yolo", "--skip-git-repo-check", prompt_text])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        timeout_s = _coerce_timeout(node.attrs.get("timeout", "1800"), 1800)
        try:
            proc = subprocess.run(
                args,
                cwd=ctx.workdir,
                capture_output=True,
                text=True,
                timeout=timeout_s,
                check=False,
                input="",
                env=_sanitized_env(),
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
        timeout_s = _coerce_timeout(node.attrs.get("timeout", "600"), 600)
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
        args = _sandboxed_args([
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
            env=_sanitized_env(),
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
            metrics = _codergen_metrics(stdout, stderr, wall_ms)
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
        metrics = _codergen_metrics(stdout, stderr, wall_ms)
        meta = {"returncode": str(proc.returncode)}
        meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
        return Result(outcome=outcome, output=output, metadata=meta)
    else:
        return Result(outcome="failure", output=f"unknown backend {backend!r}")

    output = proc.stdout + ("\nSTDERR:\n" + proc.stderr if proc.stderr else "")
    outcome = "success" if proc.returncode == 0 else "failure"
    if backend == "agy" and output.strip().startswith("Error: timed out waiting for response"):
        outcome = "failure"
    wall_ms = int((time.monotonic() - _start_ts) * 1000)
    metrics = _codergen_metrics(proc.stdout, proc.stderr, wall_ms)
    meta = {"returncode": str(proc.returncode)}
    meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
    return Result(
        outcome=outcome,
        output=output,
        metadata=meta,
    )


def _conditional(node: Node, ctx: Context) -> Result:
    """A `shape=hexagon` decision node. The outcome comes from state."""
    key = node.attrs.get("decision_key", node.name)
    outcome = ctx.state.get(key, "success")
    return Result(outcome=outcome, output=f"decision({key})={outcome}")


def _substitute_state(text: str, ctx: Context) -> str:
    """Replace `${state.<key>}` markers in `text` from ctx.state.

    Unresolved markers are left intact so a downstream subprocess will see
    them (and typically fail visibly) rather than silently substituting "".
    """
    if "${state." not in text:
        return text
    for k, v in ctx.state.items():
        text = text.replace("${state." + k + "}", str(v))
    return text


def _path_attr(node: Node, ctx: Context, key: str, default: pathlib.Path) -> pathlib.Path:
    raw = node.attrs.get(key)
    if not raw:
        return default
    raw = _substitute_state(raw, ctx)
    if "${state." in raw:
        return default
    path = pathlib.Path(raw).expanduser()
    if not path.is_absolute():
        path = (ctx.workdir / path).resolve()
    return path


def _has_unresolved_state_placeholder(value: str) -> bool:
    return "${state." in value


def _tool(node: Node, ctx: Context) -> Result:
    """Shell out to a deterministic command supplied via `command="..."`.

    Supports `${state.<key>}` substitution in both `command` and the optional
    `cwd` attribute. `cwd` lets the node target a directory other than
    `ctx.workdir` (e.g. an AO worker's worktree path stashed in state).
    """
    cmd = node.attrs.get("command")
    if not cmd:
        return Result(outcome="failure", output="no command attribute")
    cmd = _substitute_state(cmd, ctx)
    timeout = _coerce_timeout(node.attrs.get("timeout", "300"), 300)
    cwd_attr = node.attrs.get("cwd")
    if cwd_attr:
        cwd_attr = _substitute_state(cwd_attr, ctx)
        if "${state." in cwd_attr:
            # Unresolved placeholder — backend didn't set the state key.
            # Fall back to ctx.workdir so the pipeline still works under
            # backends that don't populate it (e.g. echo / claude).
            cwd = ctx.workdir
        else:
            cwd = _path_attr(node, ctx, "cwd", ctx.workdir)
            if not cwd.exists():
                return Result(outcome="failure", output=f"cwd does not exist: {cwd}")
    else:
        cwd = ctx.workdir
    args = _sandboxed_args(shlex.split(cmd))
    if args is None:
        return Result(outcome="failure", output="sandbox-exec unavailable")
    try:
        proc = subprocess.run(
            args,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            env=_sanitized_env(),
        )
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or ""
        stderr = exc.stderr or ""
        return Result(
            outcome="failure",
            output=(stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip() or f"tool command timed out after {timeout} seconds",
            metadata={
                "command": cmd,
                "timeout": str(timeout),
                "timed_out": "true",
                "returncode": "",
            },
        )
    except Exception as exc:
        return Result(
            outcome="error",
            output=f"tool command failed: {exc}",
            metadata={
                "command": cmd,
                "timed_out": "false",
                "timeout": str(timeout),
                "returncode": "",
            },
        )
    outcome = "success" if proc.returncode == 0 else "failure"
    return Result(
        outcome=outcome,
        output=proc.stdout + ("\nSTDERR:\n" + proc.stderr if proc.stderr else ""),
        metadata={
            "command": cmd,
            "returncode": str(proc.returncode),
            "timed_out": "false",
        },
    )


def _human_gate(node: Node, ctx: Context) -> Result:
    """Pause for human approval. Reads `outcome` from stdin in interactive mode,
    or accepts a pre-seeded answer via ctx.state['<node>.outcome']."""
    pre = ctx.state.get(f"{node.name}.outcome")
    if pre is not None:
        return Result(outcome=pre, output=f"pre-seeded outcome={pre}")

    if not os.isatty(0):
        return Result(outcome="failure", output="human gate reached non-interactively")

    print(f"\n[HUMAN GATE] {node.name}: {node.attrs.get('label', node.name)}")
    answer = input("approve? (success/failure): ").strip().lower() or "success"
    return Result(outcome=answer, output=f"human={answer}")


# ---------------------------------------------------------------------------
# Token + wall-clock metrics for codergen subprocesses (orch-qhez)
# ---------------------------------------------------------------------------

# Anchored regexes for the most common token-banner shapes emitted by claude /
# codex / ao workers. We tolerate either snake_case or space-separated and
# decimal separators. Each pattern captures a single integer.
_TOKEN_IN_RE = re.compile(
    r"(?:input[_ ]tokens|prompt[_ ]tokens|tokens[_ ]in)\s*[:=]\s*(\d[\d_,]*)",
    re.IGNORECASE,
)
_TOKEN_OUT_RE = re.compile(
    r"(?:output[_ ]tokens|completion[_ ]tokens|tokens[_ ]out)\s*[:=]\s*(\d[\d_,]*)",
    re.IGNORECASE,
)
_TOKEN_TOTAL_RE = re.compile(
    r"(?:total[_ ]tokens|tokens)\s*[:=]\s*(\d[\d_,]*)",
    re.IGNORECASE,
)
_COST_RE = re.compile(
    r"(?:cost[_ ]usd|total[_ ]cost|cost)\s*[:=]\s*\$?\s*(\d+(?:\.\d+)?)",
    re.IGNORECASE,
)


def _parse_int(raw: str) -> Optional[int]:
    if raw is None:
        return None
    cleaned = raw.replace("_", "").replace(",", "")
    try:
        return int(cleaned)
    except (TypeError, ValueError):
        return None


def _last_match(pattern: re.Pattern[str], *bodies: str) -> Optional[str]:
    """Return the last regex match across the supplied bodies, or None.

    Token banners are typically the FINAL summary line — we prefer the last
    occurrence so progress chatter (e.g. streaming usage updates) doesn't win
    over the authoritative final number.
    """
    last: Optional[str] = None
    for body in bodies:
        if not body:
            continue
        for m in pattern.finditer(body):
            last = m.group(1)
    return last


def _codergen_metrics(stdout: str, stderr: str, wall_ms: int) -> dict:
    """Best-effort extraction of token/cost metrics from a backend subprocess.

    The contract is:
      * `wall_ms` is always populated (callers measure it).
      * `tokens_in`, `tokens_out`, `cost_usd` may each be `None` when the
        backend did not emit a recognised banner. Returning `None` (vs. zero)
        is important so aggregates don't silently undercount.

    Strategy: scan stdout first, then stderr, and keep the LAST hit so a
    final usage summary wins over intermediate progress prints. If only a
    total-tokens banner is found and no explicit input/output split, we
    record it under `tokens_total` so the Healer can still aggregate.
    """
    tokens_in = _parse_int(_last_match(_TOKEN_IN_RE, stdout, stderr))
    tokens_out = _parse_int(_last_match(_TOKEN_OUT_RE, stdout, stderr))
    cost_raw = _last_match(_COST_RE, stdout, stderr)
    cost_usd: Optional[float] = None
    if cost_raw is not None:
        try:
            cost_usd = float(cost_raw)
        except (TypeError, ValueError):
            cost_usd = None

    metrics: dict = {"wall_ms": int(wall_ms)}
    # Only include keys whose value is meaningful — None is allowed so
    # downstream consumers can distinguish "absent" from "zero".
    metrics["tokens_in"] = tokens_in
    metrics["tokens_out"] = tokens_out
    metrics["cost_usd"] = cost_usd

    # If no explicit in/out split was found but a generic "tokens=" or
    # "total_tokens=" was, surface it under tokens_total for the Healer.
    if tokens_in is None and tokens_out is None:
        total_raw = _last_match(_TOKEN_TOTAL_RE, stdout, stderr)
        total = _parse_int(total_raw)
        if total is not None:
            metrics["tokens_total"] = total
    return metrics


def _claude_json_result(stdout: str, stderr: str, wall_ms: int) -> tuple[str, dict]:
    """Parse a ``claude --print --output-format json`` envelope.

    The codergen claude branch requests JSON output specifically so the coder's
    token usage and dollar cost are observable (plain ``--print`` text emits no
    usage banner, leaving the cost axis blind). Returns ``(output_text, metrics)``
    where ``output_text`` is the human-readable ``result`` field and ``metrics``
    carries authoritative ``tokens_in`` / ``tokens_out`` / ``cost_usd`` pulled
    from the envelope's ``usage`` / ``total_cost_usd``.

    Robust fallback: when stdout is not the expected JSON object (older CLI, an
    error preamble, a non-JSON crash), we fall back to the raw text plus the
    regex-based ``_codergen_metrics`` so the contract (wall_ms always, tokens
    None-not-zero when absent) still holds.
    """
    metrics = _codergen_metrics(stdout, stderr, wall_ms)
    output_text = stdout
    stripped = stdout.strip()
    if stripped.startswith("{"):
        try:
            envelope = json.loads(stripped)
        except (ValueError, TypeError):
            envelope = None
        if isinstance(envelope, dict):
            result_text = envelope.get("result")
            if isinstance(result_text, str):
                output_text = result_text
            usage = envelope.get("usage")
            if isinstance(usage, dict):
                # `input_tokens` is only the FRESH (uncached) input; with prompt
                # caching most input lands in cache_read / cache_creation. The
                # honest "tokens the coder made the model process" is the sum, so
                # tokens_in counts all three. The cache split is kept separately
                # for transparency. (cost_usd below is the cache-priced truth.)
                fresh = usage.get("input_tokens")
                cache_read = usage.get("cache_read_input_tokens")
                cache_create = usage.get("cache_creation_input_tokens")
                parts = [v for v in (fresh, cache_read, cache_create) if isinstance(v, int)]
                if parts:
                    metrics["tokens_in"] = sum(parts)
                    if isinstance(fresh, int):
                        metrics["tokens_in_fresh"] = fresh
                    if isinstance(cache_read, int):
                        metrics["tokens_cache_read"] = cache_read
                    if isinstance(cache_create, int):
                        metrics["tokens_cache_create"] = cache_create
                to = usage.get("output_tokens")
                if isinstance(to, int):
                    metrics["tokens_out"] = to
            cost = envelope.get("total_cost_usd")
            if isinstance(cost, (int, float)):
                metrics["cost_usd"] = float(cost)
    return output_text, metrics


_VERDICT_NORMALIZE = {
    "pass": "success",
    "warn": "success",
    "fail": "failure",
    "partial": "failure",
    "inconclusive": "failure",
    "insufficient": "failure",
    "invalid": "failure",
    "incomplete": "failure",
    "conditional": "failure",  # non-standard verdict (architectural concern) → failure
}

# A gate response must echo back `head_sha: <40-hex>` so we can bind the
# verdict to the exact worktree SHA the gate was meant to review. Without
# this binding a late-arriving verdict could be applied to a different
# commit. Missing/mismatched echo → outcome=error (NOT failure — distinct
# so the Healer clusters it as an infra issue, like rc!=0 + unknown verdict).
_HEAD_SHA_ECHO_RE = re.compile(
    r"head_sha\s*:\s*([0-9a-fA-F]{40})\b",
    re.IGNORECASE,
)


def _worktree_head_sha(workdir: pathlib.Path) -> Optional[str]:
    """Return the full 40-char HEAD SHA for `workdir`, or None on failure."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(workdir), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    sha = proc.stdout.strip()
    if len(sha) == 40 and all(c in "0123456789abcdef" for c in sha.lower()):
        return sha.lower()
    return None


def _verify_head_sha_echo(text: str, expected_sha: str) -> tuple[bool, str]:
    """Check that `text` contains a `head_sha: <expected>` line.

    Returns (ok, observed_sha). ok=True iff the echoed SHA matches expected.
    observed_sha is "" if no head_sha line is present.
    """
    matches = list(_HEAD_SHA_ECHO_RE.finditer(text or ""))
    if not matches:
        return False, ""
    # If multiple appear, take the LAST one — same convention as verdict parsing.
    observed = matches[-1].group(1).lower()
    return observed == expected_sha.lower(), observed

# The set of recognized verdict tokens, shared by the marker + standalone regexes.
_VERDICT_TOKEN = (
    r"(?:pass|warn|fail|partial|inconclusive|insufficient|invalid|incomplete|conditional)"
)

# Anchored regex: a verdict token must follow a marker ("verdict:", "overall:",
# "normalized:") on the same line. The gap between the marker and the captured
# token may contain ONLY decoration (whitespace, markdown like ``**``, emoji —
# any non-word char) and *qualifier verdict-tokens* (e.g. "CONDITIONAL PASS",
# "PARTIAL PASS"); backtracking captures the LAST token. It must NOT contain
# arbitrary alphabetic prose — a bare ``[^\n]*`` wildcard would lift "fail" out
# of "verdict: not a fail", which is precisely the misclassification the
# hardening tests forbid. Non-token word runs ("not", "a") break the match, so
# the caller falls through to the "marker present but invalid" → unknown path.
_MARKER_RE = re.compile(
    r"(?:verdict|overall|normalized)\s*:\s*"
    r"(?:" + _VERDICT_TOKEN + r"\b|[^\w\n])*"
    r"(" + _VERDICT_TOKEN + r")\b",
    re.IGNORECASE,
)

# Bare marker (presence of "verdict:"/"overall:"/"normalized:" anywhere) — used to
# detect that the gate *attempted* to emit a verdict line. If that's present we
# trust only the regex above; we don't fall back to scanning the whole tail,
# because the fallback can lift "fail" out of compound phrases like
# "verdict: not a fail".
_MARKER_PRESENT_RE = re.compile(
    r"(?:verdict|overall|normalized)\s*:",
    re.IGNORECASE,
)

# Fallback: a verdict line standing alone (whitespace + token + optional
# trailing punctuation). Stricter than a free `\b` scan so prose like
# "not a fail" doesn't slip through.
_STANDALONE_RE = re.compile(
    r"^\s*(pass|warn|fail|partial|inconclusive|conditional)\b[\s.!:]*$",
    re.IGNORECASE | re.MULTILINE,
)


def _parse_verdict(text: str) -> tuple[str, str]:
    """Extract a normalized verdict from gate output.

    Strategy:
      1. Look for explicit marker lines (`Verdict: PASS`). The LAST valid marker
         wins — gates may emit progress lines before the authoritative one.
      2. If a marker word was present but no matching token followed it,
         return ("unknown", "failure") — do NOT fall back; the gate's own
         marker line is the contract.
      3. With no marker at all, scan the last 40 lines for a *standalone*
         verdict token (not embedded in prose).

    Returns (raw_verdict, normalized_outcome). Unknown returns ("unknown", "failure").
    """
    body = text or ""
    matches = list(_MARKER_RE.finditer(body))
    if matches:
        raw = matches[-1].group(1).lower()
        return raw, _VERDICT_NORMALIZE.get(raw, "failure")

    if _MARKER_PRESENT_RE.search(body):
        # A verdict marker existed but with an invalid token — refuse to guess.
        return "unknown", "failure"

    tail = "\n".join(body.splitlines()[-40:])
    fallback = list(_STANDALONE_RE.finditer(tail))
    if fallback:
        raw = fallback[-1].group(1).lower()
        return raw, _VERDICT_NORMALIZE.get(raw, "failure")
    return "unknown", "failure"


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
        return _sandboxed_args([
            "agy",
            "--add-dir", str(ctx.workdir),
            "--dangerously-skip-permissions",
            "--print-timeout", f"{timeout}s",
            "--print",
            prompt,
        ])
    if backend == "codex":
        return _sandboxed_args([
            "codex", "exec", "--yolo", "--skip-git-repo-check", prompt,
        ])
    # ``claude-sonnet`` (priority-queue name), bare ``claude`` (run-level
    # default), and any other claude-routed backend → Anthropic Claude CLI.
    # ``minimax`` is a special case of this path with a different base URL
    # (see ``_gate_subprocess_env``).
    claude_bin = _get_claude_executable()
    return _sandboxed_args([claude_bin, "--print", "--dangerously-skip-permissions", prompt])


def _gate_subprocess_env(backend: str) -> dict[str, str]:
    """Env overrides for a reviewer-gate subprocess on ``backend``.

    For ``minimax`` the Claude CLI must route through the minimax Anthropic-
    compatible gateway; ``ANTHROPIC_BASE_URL`` is the only override, layered
    on top of ``_sanitized_env`` (never raw ``os.environ`` — holdout vars
    must not reach any reviewer subprocess). All other backends use
    ``_sanitized_env`` unchanged.
    """
    if backend == "minimax":
        return {**_sanitized_env(), "ANTHROPIC_BASE_URL": "https://api.minimax.io/anthropic"}
    return _sanitized_env()


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
    verdict, normalized = _parse_verdict(combined)
    # SHA binding check comes BEFORE collapsing to pass/fail so a spoofed-pass
    # with the wrong SHA collapses to `error`, not `success`.
    sha_ok, observed_sha = _verify_head_sha_echo(combined, expected_sha)
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
        if _probe_backend_installed(name):
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
    ``{"reviewer_backend_resolution": "run_level"}``. Callers merge this into
    the gate ``Result.metadata`` so the operator/CXDB can see exactly why a
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


def _slash_gate(slash_command: str, default_args: str = "") -> Handler:
    """Build a handler that shells out to the reviewer backend with `/<command> <args>`."""

    def handler(node: Node, ctx: Context) -> Result:
        args = node.attrs.get("args", default_args)
        target = node.attrs.get("target", str(ctx.workdir))

        # Echo backend: outcome from state hint, used by tests + CI. The
        # echo path does not call out to a subprocess so SHA binding is not
        # applicable — tests that exercise SHA binding must use the claude
        # backend with a fake binary on PATH.
        if ctx.backend in ("echo", "mock_llm"):
            hint = ctx.state.get(f"{node.name}.outcome", "success")
            return Result(
                outcome=hint,
                output=f"echo gate /{slash_command}: pre-seeded {hint}",
                metadata={"slash_command": slash_command, "verdict": "echo:" + hint},
            )

        # SHA binding: compute the worktree HEAD and require the gate to
        # echo it back. If we cannot compute a SHA (not a git worktree, git
        # missing), the gate cannot be safely run — return `error`.
        expected_sha = _worktree_head_sha(ctx.workdir)
        if expected_sha is None:
            return Result(
                outcome="error",
                output=f"gate /{slash_command} cannot resolve HEAD SHA for {ctx.workdir}",
                metadata={
                    "slash_command": slash_command,
                    "verdict": "unknown",
                    "head_sha_status": "missing",
                },
            )

        sha_directive = (
            f"\n\n"
            f"<!-- RUNNER BINDING REQUIREMENT (non-negotiable) -->\n"
            f"expected_head_sha: {expected_sha}\n\n"
            f"CRITICAL: Your response MUST include the following line verbatim "
            f"(machine-parsed binding — do NOT omit it, paraphrase it, "
            f"or put it inside a code block):\n"
            f"head_sha: {expected_sha}\n\n"
            f"Place this line near the top of your response, right after any "
            f"header/context block. The pipeline runner rejects responses missing this line.\n"
        )
        prompt = f"/{slash_command} {args} {target}".strip() + sha_directive
        timeout = _coerce_timeout(node.attrs.get("timeout", "1200"), 1200)

        # Reviewer backend routing + agy→claude infra fallback live in
        # _execute_gate. The gate is read-only and SHA-bound regardless of
        # which backend grades the diff.
        backend, gate_meta = _resolve_gate_backend(node, ctx)
        result = _execute_gate(prompt, expected_sha, timeout, ctx, slash_command, backend)
        if gate_meta:
            for k, v in gate_meta.items():
                result.metadata.setdefault(k, v)
        return result

    handler.__name__ = f"_gate_{slash_command}"  # noqa: WPS125
    return handler


UNIVERSAL_CODE_STANDARDS_PROMPT = """\
You are performing an automated, repository-agnostic Code Standards & Quality Review.
Analyze the active repository changes and diff in the current workspace.

You MUST audit the implementation against the following core principles:

1. ZERO FRAMEWORK COGNITION (ZFC):
   - Avoid dependency/framework bloat. Prioritize lightweight, native logical primitives.
   - Do not pull in heavy abstractions or third-party wrappers unnecessarily.
   - Ensure the solution uses standard library/native features where applicable.

2. ROOT-CAUSE-FIRST ENGINEERING:
   - Verify that the changes directly address the true root cause of the feature request or bug.
   - Look for and reject "workaround" logic, such as generic try/catch suppression, defensive
     fallback values, sanitizers, or clamp layers that merely mask upstream errors.
   - Prohibit fixing symptoms when upstream prompts, schemas, or instructions should be corrected.

3. CLEAN CODE & CODE QUALITY:
   - Readability, proper modularity, clean interface boundaries, and appropriate type annotations.
   - No trailing debugging logs/comments or placeholders.

Provide a detailed review report listing:
- A brief summary of scope.
- Audit results for ZFC, Root-Cause-First, and Clean Code.
- A bulleted list of any blockers and required fixes.

CRITICAL FORMATTING INSTRUCTIONS:
1. You MUST include a binding verification line:
   head_sha: {expected_sha}

2. You MUST conclude your review with:
   verdict: <pass|warn|fail>
"""


UNIVERSAL_EVIDENCE_REVIEW_PROMPT = """\
You are performing an automated, repository-agnostic Evidence Standards & Review check.
Analyze the active repository changes, diff, and any generated evidence files in the workspace.

You MUST audit the implementation's evidence against the following core standards:

1. GIT PROVENANCE & STALENESS:
   - Check if metadata captures the exact git HEAD SHA, branch, and merge base.
   - Confirm that the SHA of the recorded evidence matches the current HEAD: {expected_sha}

2. METRICS & TELEMETRY:
   - Confirm that token usage, execution duration, and costs are accurately logged.

3. RESULT INVARIANTS:
   - Ensure all executed scenarios are explicitly marked as passed with no silent failures.

4. CHECKSUM INTEGRITY:
   - Confirm that generated files are accompanied by valid checksums or integrity metadata.

CRITICAL FORMATTING INSTRUCTIONS:
1. You MUST include a binding verification line:
   head_sha: {expected_sha}

2. You MUST conclude your review with:
   verdict: <pass|fail>
"""


def _run_universal_prompt_gate(
    prompt_template: str, name: str, node: "Node", ctx: "Context"
) -> "Result":
    """Run a gate review using an embedded universal prompt (no local slash command file)."""
    if ctx.backend in ("echo", "mock_llm"):
        hint = ctx.state.get(f"{node.name}.outcome", "success")
        return Result(
            outcome=hint,
            output=f"echo gate {name}: pre-seeded {hint}",
            metadata={"slash_command": name, "verdict": "echo:" + hint},
        )

    expected_sha = _worktree_head_sha(ctx.workdir)
    if expected_sha is None:
        return Result(
            outcome="error",
            output=f"gate {name} cannot resolve HEAD SHA for {ctx.workdir}",
            metadata={"slash_command": name, "verdict": "unknown", "head_sha_status": "missing"},
        )

    sha_directive = (
        f"\n\n<!-- RUNNER BINDING REQUIREMENT (non-negotiable) -->\n"
        f"expected_head_sha: {expected_sha}\n\n"
        f"CRITICAL: Your response MUST include the following line verbatim:\n"
        f"head_sha: {expected_sha}\n"
    )
    prompt = prompt_template.format(expected_sha=expected_sha) + sha_directive
    timeout = _coerce_timeout(node.attrs.get("timeout", "1200"), 1200)

    # Reviewer backend routing + agy→claude infra fallback live in
    # _execute_gate (shared with _slash_gate).
    backend, gate_meta = _resolve_gate_backend(node, ctx)
    result = _execute_gate(prompt, expected_sha, timeout, ctx, name, backend)
    if gate_meta:
        for k, v in gate_meta.items():
            result.metadata.setdefault(k, v)
    return result


def _node_prompt_ref(node: "Node") -> Optional[str]:
    """Prompt template ref from node attrs (mirrors ``Node.prompt_ref``).

    Reads ``attrs`` directly so gate routing also works for duck-typed test
    nodes that don't carry the parser property.
    """
    ref = node.attrs.get("prompt")
    if not ref:
        return None
    ref = str(ref)
    return ref[1:] if ref.startswith("@") else ref


def _run_custom_prompt_gate(node: "Node", ctx: "Context", name: str) -> "Result":
    """Run a gate review using the node's own prompt template (``prompt="@path"``).

    A graph that authors review instructions on a gate node owns the review
    *content*; the runner still owns the machine contract — SHA binding and
    the ``verdict: <pass|fail>`` marker are appended here so
    ``_parse_verdict`` / ``_verify_head_sha_echo`` grade a custom gate
    exactly like the slash and universal gates.
    """
    if ctx.backend in ("echo", "mock_llm"):
        hint = ctx.state.get(f"{node.name}.outcome", "success")
        return Result(
            outcome=hint,
            output=f"echo gate {name}: pre-seeded {hint}",
            metadata={"slash_command": name, "verdict": "echo:" + hint},
        )

    expected_sha = _worktree_head_sha(ctx.workdir)
    if expected_sha is None:
        return Result(
            outcome="error",
            output=f"gate {name} cannot resolve HEAD SHA for {ctx.workdir}",
            metadata={"slash_command": name, "verdict": "unknown", "head_sha_status": "missing"},
        )

    rendered = _render_prompt(node, ctx)
    # _render_prompt degrades to a goal-only stub when the template is
    # missing or outside the allowed roots. A coder node can limp along on
    # the stub; a review gate must not silently grade with no instructions.
    ref = _node_prompt_ref(node)
    if f"(missing prompt: {ref})" in rendered or f"(invalid prompt: {ref})" in rendered:
        return Result(
            outcome="error",
            output=f"gate {name}: prompt template unavailable: {ref}",
            metadata={"slash_command": name, "verdict": "unknown", "prompt_status": "missing"},
        )

    contract = (
        f"\n\nCRITICAL FORMATTING INSTRUCTIONS:\n"
        f"1. You MUST include a binding verification line:\n"
        f"   head_sha: {expected_sha}\n\n"
        f"2. You MUST conclude your review with:\n"
        f"   verdict: <pass|fail>\n"
    )
    sha_directive = (
        f"\n\n<!-- RUNNER BINDING REQUIREMENT (non-negotiable) -->\n"
        f"expected_head_sha: {expected_sha}\n\n"
        f"CRITICAL: Your response MUST include the following line verbatim:\n"
        f"head_sha: {expected_sha}\n"
    )
    prompt = rendered + contract + sha_directive
    timeout = _coerce_timeout(node.attrs.get("timeout", "1200"), 1200)

    # Reviewer backend routing + agy→claude infra fallback live in
    # _execute_gate (shared with _slash_gate / _run_universal_prompt_gate).
    backend, gate_meta = _resolve_gate_backend(node, ctx)
    result = _execute_gate(prompt, expected_sha, timeout, ctx, name, backend)
    if gate_meta:
        for k, v in gate_meta.items():
            result.metadata.setdefault(k, v)
    return result


def _gate_es(node: "Node", ctx: "Context") -> "Result":
    if _node_prompt_ref(node):
        return _run_custom_prompt_gate(node, ctx, "gate_es")
    local_es = ctx.workdir / ".claude" / "commands" / "es.md"
    if local_es.exists():
        return _slash_gate("es")(node, ctx)
    return _run_universal_prompt_gate(UNIVERSAL_EVIDENCE_REVIEW_PROMPT, "gate_es", node, ctx)


def _gate_er(node: "Node", ctx: "Context") -> "Result":
    if _node_prompt_ref(node):
        return _run_custom_prompt_gate(node, ctx, "gate_er")
    local_er = ctx.workdir / ".claude" / "commands" / "er.md"
    local_cmd = ctx.workdir / ".claude" / "commands" / "evidence_review.md"
    if local_er.exists():
        return _slash_gate("er")(node, ctx)
    if local_cmd.exists():
        return _slash_gate("evidence_review")(node, ctx)
    return _run_universal_prompt_gate(UNIVERSAL_EVIDENCE_REVIEW_PROMPT, "gate_er", node, ctx)


def _gate_code_standards(node: "Node", ctx: "Context") -> "Result":
    if _node_prompt_ref(node):
        return _run_custom_prompt_gate(node, ctx, "gate_code_standards")
    local_cmd = ctx.workdir / ".claude" / "commands" / "code-standards.md"
    if local_cmd.exists():
        return _slash_gate("code-standards")(node, ctx)
    return _run_universal_prompt_gate(UNIVERSAL_CODE_STANDARDS_PROMPT, "gate_code_standards", node, ctx)


def _gate_slash(node: "Node", ctx: "Context") -> "Result":
    """Generic single-lane reviewer gate: ``type="gate_slash" command="zfc"``.

    Runs ``/<command>`` on the reviewer backend with the same SHA binding and
    verdict parsing as the named gates. Built for decomposing orchestrator
    slash commands into per-lane pipeline nodes: a command like
    ``/code-standards`` instructs the *agent* to fan out several sub-reviews,
    which single-subprocess backends (``codex exec``) cannot do — they burn
    the gate timeout ingesting every lane's skill text and die before any
    verdict. With ``gate_slash`` the .dot graph owns the fan-out
    (``type=parallel`` → one ``gate_slash`` node per lane → ``type=join``),
    so each subprocess loads exactly one lane's skill.

    The named command must exist in the target repo
    (``.claude/commands/<command>.md``); otherwise the gate errors rather
    than letting the backend free-associate a review.
    """
    command = str(node.attrs.get("command", "")).strip().lstrip("/")
    if not command:
        return Result(
            outcome="error",
            output=f"gate_slash node {node.name!r} missing required command attr",
            metadata={"verdict": "unknown", "slash_command": ""},
        )
    if _node_prompt_ref(node):
        return _run_custom_prompt_gate(node, ctx, f"gate_{command}")
    # Echo/mock backends never shell out, so the command-file requirement
    # doesn't apply — let the echo branch in _slash_gate seed the outcome.
    if ctx.backend in ("echo", "mock_llm"):
        return _slash_gate(command)(node, ctx)
    local_cmd = ctx.workdir / ".claude" / "commands" / f"{command}.md"
    if not local_cmd.exists():
        # User-scope commands (~/.claude/commands/) are resolvable by the
        # claude CLI but not by every reviewer backend (codex resolves
        # in-repo files only). Materialize the command into the target
        # workdir so all backends see it repo-local.
        user_cmd = pathlib.Path.home() / ".claude" / "commands" / f"{command}.md"
        if user_cmd.exists():
            try:
                local_cmd.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(user_cmd, local_cmd)
            except OSError as exc:
                return Result(
                    outcome="error",
                    output=f"gate_slash: failed to materialize /{command} into workdir: {exc}",
                    metadata={"verdict": "unknown", "slash_command": command},
                )
        else:
            return Result(
                outcome="error",
                output=(
                    f"gate_slash: /{command} not found in target repo "
                    f"({local_cmd}) or user scope ({user_cmd}) — "
                    f"refusing to run an undefined review lane"
                ),
                metadata={"verdict": "unknown", "slash_command": command},
            )
    return _slash_gate(command)(node, ctx)


def _run_pytest_test(node: "Node", ctx: "Context", *, label: str) -> "Result":
    """Run pytest against the test path stored in node attrs or state.

    Returns a Result with ``outcome=success`` only if pytest exits 0.
    The caller maps 0/non-0 to its own semantics (red vs green).
    """
    raw_path = node.attrs.get("test_path", "${state.bug_fix.test_path}")
    test_path = _substitute_state(str(raw_path), ctx)
    if "${state." in test_path or not test_path.strip():
        return Result(
            outcome="error",
            output=f"{label}: unresolved test_path: {raw_path!r} "
                   f"(set state.bug_fix.test_path before this gate)",
        )
    pytest_args = str(node.attrs.get("pytest_args", "-x")).strip()
    # Build argv as a list (not a string + shlex.split) so paths with spaces
    # in sys.executable or test_path are preserved verbatim. shlex.split on
    # `/Applications/cmux DEV may-18.app/.../python` would break the path on
    # the space; list-form argv avoids that whole class of bug.
    args_list: list[str] = [sys.executable, "-m", "pytest", test_path]
    if pytest_args:
        args_list.extend(shlex.split(pytest_args))
    timeout = _coerce_timeout(node.attrs.get("timeout", "300"), 300)
    args = _sandboxed_args(args_list)
    if args is None:
        return Result(outcome="error", output=f"{label}: sandbox-exec unavailable")
    try:
        proc = subprocess.run(
            args,
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            env=_sanitized_env(),
        )
    except subprocess.TimeoutExpired:
        return Result(
            outcome="failure",
            output=f"{label}: pytest timed out after {timeout}s on {test_path}",
        )
    except Exception as exc:
        return Result(
            outcome="error",
            output=f"{label}: pytest invocation failed: {exc}",
        )
    return Result(
        outcome="success" if proc.returncode == 0 else "failure",
        output=(
            proc.stdout
            + ("\nSTDERR:\n" + proc.stderr if proc.stderr else "")
        ).strip(),
        metadata={
            "test_path": test_path,
            "command": " ".join(args_list),
            "returncode": str(proc.returncode),
            "label": label,
        },
    )


def _gate_red(node: "Node", ctx: "Context") -> "Result":
    """Red gate: the test MUST fail (rc != 0). Used by bug_fix.dot after the
    reproduce node writes a fresh failing test. Outcome=success means the
    bug is reproduced; outcome=failure means the test passed (bug not
    reproduced — agent shortcut or the bug report was wrong).
    """
    result = _run_pytest_test(node, ctx, label="gate_red")
    if result.outcome == "error":
        # Infra error (unresolved test_path, timeout, invocation failure) —
        # NOT a successful reproduction. Surface as failure.
        return Result(
            outcome="failure",
            output="RED FAIL: gate could not run pytest (infra error); "
                   f"this is not a successful reproduction.\n{result.output}",
        )
    if result.outcome == "success":
        # pytest exited 0 — the test passed, so the bug was NOT reproduced.
        return Result(
            outcome="failure",
            output="RED FAIL: test passed but bug was not reproduced. "
                   f"Original pytest output:\n{result.output}",
        )
    # pytest exited non-zero — the test failed as expected.
    return Result(
        outcome="success",
        output=f"RED OK: test failed as expected (rc={result.metadata.get('returncode')}).\n{result.output}",
    )


def _gate_green(node: "Node", ctx: "Context") -> "Result":
    """Green gate: the test MUST pass (rc == 0). Used by bug_fix.dot after
    the fix node applies the change. Outcome=success means the fix works;
    outcome=failure means the test is still failing.
    """
    result = _run_pytest_test(node, ctx, label="gate_green")
    if result.outcome == "error":
        return Result(
            outcome="error",
            output=f"GREEN ERROR: gate could not run pytest (infra error).\n{result.output}",
        )
    if result.outcome == "success":
        return Result(
            outcome="success",
            output=f"GREEN OK: test passed after fix (rc=0).\n{result.output}",
        )
    return Result(
        outcome="failure",
        output=f"GREEN FAIL: test still failing after fix (rc={result.metadata.get('returncode')}).\n{result.output}",
    )


def _tcp_port_open(host: str, port: int, timeout: float = 1.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def _holdout_eval(node: Node, ctx: Context) -> Result:
    """Run the sealed holdout evaluator in a separate process.

    Infrastructure invariants (see memory/feedback_2026-05-24_holdout_eval_emulator_infra.md):
    1. Java on PATH for Firebase emulators (Homebrew openjdk path).
    2. Poll TCP ports, don't sleep — wait for all emulators to be ready.
    3. Kill process GROUP on cleanup, not just the wrapper process.
    4. Strip real GCP credentials from env so Cloud Functions emulator uses local project.
    5. Pre-clean emulator ports before launching to kill stale JVM holders.
    6. Run seed script (impl/scripts/seed.ts or npm run seed) after emulators are ready.
    """
    import random

    repo_path = _holdouts_repo_path()
    node_feature = node.attrs.get("feature")
    feature = str(ctx.state.get("feature", "")) or (str(node_feature) if node_feature is not None else "")
    if isinstance(feature, str) and "${state." in feature:
        feature = _substitute_state(feature, ctx)
        if "${state." in feature:
            return Result(outcome="failure", output=f"unresolved feature path: {node_feature!r}")
    feature = str(feature or "").strip()
    if not feature:
        return Result(outcome="failure", output="no feature attribute or state")

    eval_script = repo_path / "evaluator" / "run.py"
    try:
        exists = eval_script.exists()
    except PermissionError:
        exists = False
    if not exists:
        return Result(outcome="failure", output=f"holdout evaluator missing: {eval_script}")

    impl_attr = node.attrs.get("implementation")
    if impl_attr:
        resolved = _substitute_state(impl_attr, ctx)
        if _has_unresolved_state_placeholder(resolved):
            return Result(outcome="failure", output=f"unresolved implementation path: {impl_attr}")

    impl = _path_attr(node, ctx, "implementation", ctx.workdir)
    if not impl.exists():
        return Result(outcome="failure", output=f"implementation missing: {impl}")

    port = random.randint(30001, 30999)

    # Build eval env from the sanitized environment: the server/seed
    # subprocesses below run agent-authored code (make run, npm seed,
    # scripts/seed.*), so DARK_FACTORY_HOLDOUTS / *HOLDOUT* must never reach
    # them — a seed script could copy holdout content into the worktree for
    # the next fix-loop iteration. The sealed evaluator does not need the
    # variable either (it resolves scenarios relative to its own repo path
    # and strips holdout vars from its own children).
    eval_env = _sanitized_env()

    # Fix 1 — Java PATH: prepend Homebrew openjdk so Firebase emulators can find java.
    homebrew_java = "/opt/homebrew/opt/java/bin"
    if os.path.isdir(homebrew_java):
        eval_env["PATH"] = homebrew_java + ":" + eval_env.get("PATH", "")
        eval_env["JAVA_HOME"] = "/opt/homebrew/opt/java"

    # Fix 4 — Strip real GCP credentials: Cloud Functions emulator must use local project.
    for gcp_var in ("GOOGLE_APPLICATION_CREDENTIALS", "GCLOUD_PROJECT", "GOOGLE_CLOUD_PROJECT"):
        eval_env.pop(gcp_var, None)

    eval_env["BENCHMARK_PORT"] = str(port)
    # Strip real GCP credentials so Firebase emulators don't try to reach
    # production — they should use emulator-local auth only.
    for _cred_key in ("GOOGLE_APPLICATION_CREDENTIALS", "GCLOUD_PROJECT",
                      "GOOGLE_CLOUD_PROJECT"):
        eval_env.pop(_cred_key, None)

    startup_delay = int(node.attrs.get("startup_delay", "5"))
    server_proc = None
    makefile = impl / "Makefile"
    firebase_json = impl / "firebase.json"
    has_make_run = False
    if makefile.exists():
        try:
            has_make_run = bool(re.search(r"^run\s*:", makefile.read_text(), re.MULTILINE))
        except Exception:
            pass
    if has_make_run:
        env_p = dict(eval_env)
        env_p["PORT"] = str(port)
        # Fix 3 — start_new_session=True so we can killpg the whole JVM tree.
        server_proc = subprocess.Popen(
            ["make", "run"], cwd=str(impl), env=env_p,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        time.sleep(startup_delay)
    elif firebase_json.exists():
        # Kill any lingering processes from previous runs that hold the
        # Firebase emulator ports, so the new emulator can bind cleanly.
        for _em_port in (8080, 9099, 5001, 4000, 4400):
            try:
                _lsof = subprocess.run(
                    ["lsof", "-ti", f":{_em_port}"],
                    capture_output=True, text=True, timeout=5)
                for _pid_s in _lsof.stdout.strip().split():
                    try:
                        os.kill(int(_pid_s), signal.SIGTERM)
                    except (ProcessLookupError, ValueError):
                        pass
            except Exception:
                pass
        time.sleep(2)
        # Ensure Java is on PATH — Firebase emulators require it.
        # Homebrew installs Java at /opt/homebrew/opt/java/bin but doesn't
        # add it to PATH by default. Prepend it so emulators can find `java`.
        _homebrew_java = "/opt/homebrew/opt/java/bin"
        _java_path = eval_env.get("PATH", "")
        if _homebrew_java not in _java_path:
            eval_env["PATH"] = _homebrew_java + ":" + _java_path
        eval_env.setdefault("JAVA_HOME", "/opt/homebrew/opt/java")
        # Build Cloud Functions if source exists but compiled output is missing
        fn_pkg = impl / "functions" / "package.json"
        fn_lib = impl / "functions" / "lib" / "index.js"
        if fn_pkg.exists() and not fn_lib.exists():
            subprocess.run(
                ["npm", "install", "--prefix", str(impl / "functions"), "--silent"],
                cwd=str(impl), capture_output=True, timeout=120)
            subprocess.run(
                ["npm", "run", "build", "--prefix", str(impl / "functions")],
                cwd=str(impl), capture_output=True, timeout=120)
        server_proc = subprocess.Popen(
            ["firebase", "emulators:start",
             "--only", "firestore,auth,storage,functions"],
            cwd=str(impl), env=dict(eval_env),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        # Fix 5b — atexit cleanup so orphan JVMs are reaped even on SIGKILL (orch-2fze).
        import atexit as _atexit
        _atexit_pgid: list[int | None] = [None]
        try:
            _atexit_pgid[0] = os.getpgid(server_proc.pid)
        except Exception:
            pass

        def _kill_emulator_group(_pgid_ref: list = _atexit_pgid) -> None:
            pgid = _pgid_ref[0]
            if pgid is not None:
                try:
                    os.killpg(pgid, signal.SIGTERM)
                except Exception:
                    pass

        _atexit.register(_kill_emulator_group)
        # Poll until all required emulator ports respond or startup_delay expires.
        _emulator_ports = [8080, 9099, 5001]
        _deadline = time.monotonic() + startup_delay
        while time.monotonic() < _deadline:
            if all(_tcp_port_open("localhost", p) for p in _emulator_ports):
                break
            time.sleep(2)

        # Fix 6 — seed emulator with baseline data before evaluator runs (orch-0bne).
        _seed_pkg = impl / "package.json"
        _seed_ts = impl / "scripts" / "seed.ts"
        _seed_js = impl / "scripts" / "seed.js"
        _seeded = False
        if _seed_pkg.exists():
            try:
                _pkg_data = json.loads(_seed_pkg.read_text())
                if "seed" in _pkg_data.get("scripts", {}):
                    subprocess.run(
                        ["npm", "run", "seed"],
                        cwd=str(impl), env=dict(eval_env),
                        capture_output=True, timeout=30, check=False)
                    _seeded = True
            except Exception:
                pass
        if not _seeded and _seed_ts.exists():
            try:
                subprocess.run(
                    ["npx", "ts-node", str(_seed_ts)],
                    cwd=str(impl), env=dict(eval_env),
                    capture_output=True, timeout=30, check=False)
                _seeded = True
            except Exception:
                pass
        if not _seeded and _seed_js.exists():
            try:
                subprocess.run(
                    ["node", str(_seed_js)],
                    cwd=str(impl), env=dict(eval_env),
                    capture_output=True, timeout=30, check=False)
            except Exception:
                pass

    try:
        proc = subprocess.run(
            ["python3", str(eval_script), "--feature", feature, "--impl", str(impl)],
            cwd=repo_path, capture_output=True, text=True, timeout=600, check=False, env=eval_env)
        verdict = "failure"
        summary = {
            "verdict": verdict,
            "passed": 0,
            "total": 0,
            "status_counts": {},
            "sealed": True,
        }
        for line in reversed(proc.stdout.splitlines()):
            if line.strip().startswith("{") and line.strip().endswith("}"):
                try:
                    data = json.loads(line.strip())
                    verdict = data.get("verdict", "failure").lower()
                    scenarios = data.get("scenarios", [])
                    status_counts: dict[str, int] = {}
                    for sc in scenarios:
                        status = str(sc.get("status", "unknown"))
                        status_counts[status] = status_counts.get(status, 0) + 1
                    passed = status_counts.get("pass", 0)
                    total = len(scenarios)
                    summary = {
                        "verdict": verdict,
                        "passed": passed,
                        "total": total,
                        "status_counts": status_counts,
                        "sealed": True,
                    }

                    # Write only redacted holdout results into the implementation
                    # tree. Per-scenario data remains sealed in the evaluator.
                    results_file = impl / "results" / "holdout_results.json"
                    results_file.parent.mkdir(exist_ok=True)
                    results_file.write_text(json.dumps(summary, indent=2))

                    break
                except: pass
        # rc!=0 + verdict=pass means the evaluator process crashed/exited
        # abnormally even though it printed a pass verdict line — that's a
        # spoof attempt or infra bug, not a real pass. Route to "error" so
        # the engine can route via outcome!=success edges and the Healer
        # clusters infra crashes separately from real failures.
        if proc.returncode and verdict == "pass":
            outcome = "error"
            summary = {**summary, "verdict": "error", "returncode": proc.returncode}
        elif verdict == "pass":
            outcome = "success"
        else:
            outcome = verdict
        return Result(
            outcome=outcome,
            output=json.dumps(summary, indent=2),
            metadata={"verdict": verdict, "port": str(port), "sealed": "true"},
        )
    finally:
        if server_proc:
            # Kill the entire process group so JVM child processes (Firestore
            # emulator) are terminated along with the firebase CLI wrapper.
            # start_new_session=True gives the process its own session, so
            # os.killpg on the process group reaps all children.
            try:
                pgid = os.getpgid(server_proc.pid)
                os.killpg(pgid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                server_proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    pgid = os.getpgid(server_proc.pid)
                    os.killpg(pgid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

def _render_prompt(node: Node, ctx: Context) -> str:
    ref = node.prompt_ref
    if not ref:
        return f"# {node.name}\n\nGoal: {ctx.goal}"
    ref_path = pathlib.Path(ref)
    if ref_path.is_absolute():
        resolved_ref = ref_path
        try:
            resolved_ref = ref_path.resolve()
        except FileNotFoundError:
            return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"

        for deny in _holdout_denied_paths():
            try:
                resolved_ref.relative_to(deny)
            except ValueError:
                pass
            else:
                return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"

        text_path = resolved_ref
        if not text_path.exists():
            return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"
        text = text_path.read_text()
        text = text.replace("${goal}", ctx.goal)
        for k, v in ctx.state.items():
            text = text.replace("${state." + k + "}", v)
        return text
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
                return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"
        else:
            return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"
    if not p.exists():
        return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"
    text = p.read_text()
    text = text.replace("${goal}", ctx.goal)
    for k, v in ctx.state.items():
        text = text.replace("${state." + k + "}", v)
    return text


def _parallel_fanout(node: Node, ctx: Context) -> Result:
    """Fan-out handler — records the fan-out step; actual concurrent branching is in engine.py."""
    return Result(outcome="success", output=f"fanout: {node.name}", metadata={"role": "fanout"})


def _join_handler(node: Node, ctx: Context) -> Result:
    """Join handler — signals the node type; policy evaluation is in engine.py.

    The engine's parallel block calls _apply_join_policy and builds the join
    StepRecord directly, so this handler is never invoked for join nodes that
    follow a type=parallel fan-out.  If a join node is reached via normal
    (non-parallel) traversal, there are no branches to aggregate and
    returning success is correct.
    """
    return Result(outcome="success", output=f"join: {node.name}", metadata={"role": "join"})


REGISTRY: dict[str, Handler] = {
    # by shape
    "Mdiamond": _start,
    "Msquare": _exit,
    "hexagon": _conditional,
    "component": _parallel_fanout,      # fan-out shape alias (Kilroy: shape=component)
    "tripleoctagon": _join_handler,     # join shape alias (Kilroy: shape=tripleoctagon)
}

TYPE_REGISTRY: dict[str, Handler] = {
    "start": _start,
    "exit": _exit,
    "codergen": _codergen,
    "conditional": _conditional,
    "tool": _tool,
    "human_gate": _human_gate,
    "holdout_eval": _holdout_eval,
    "gate_es": _gate_es,
    "gate_er": _gate_er,
    "gate_code_standards": _gate_code_standards,
    "gate_slash": _gate_slash,
    "gate_red": _gate_red,
    "gate_green": _gate_green,
    "parallel": _parallel_fanout,       # fan-out type (type=parallel)
    "join": _join_handler,              # fan-in type (type=join)
}


def resolve(node: Node) -> Handler:
    t = node.attrs.get("type")
    if t and t in TYPE_REGISTRY:
        return TYPE_REGISTRY[t]
    if is_start_node(node):
        return _start
    if is_exit_node(node):
        return _exit
    s = node.shape
    if s in REGISTRY:
        return REGISTRY[s]
    return _codergen
