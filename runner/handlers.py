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
import time
from dataclasses import dataclass, field
from typing import Callable, Optional

from .parser import Node, is_start_node, is_exit_node


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
        return Result(outcome="success", output=prompt_text, metadata=meta)

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
            proc = subprocess.run(
                spawn_args,
                cwd=ctx.workdir,
                capture_output=True,
                text=True,
                timeout=300,
                check=False,
                env=_sanitized_env(),
            )
            if proc.returncode != 0:
                return Result(
                    outcome="failure",
                    output=f"ao spawn failed (rc={proc.returncode})\n{proc.stdout}\nSTDERR:\n{proc.stderr}",
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
            activity = _ao_wait_idle(sess_name, ctx.workdir, timeout=900, project=project)
            outcome = "success" if activity in ("exited", "ready") else "failure"
            wall_ms = int((time.monotonic() - _start_ts) * 1000)
            metrics = _codergen_metrics(proc.stdout, proc.stderr, wall_ms)
            meta = {"session": sess_name, "worktree": worktree or "", "activity": activity}
            meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
            return Result(
                outcome=outcome,
                output=f"ao spawn session={sess_name} worktree={worktree} activity={activity}",
                metadata=meta,
            )

        send_args = _sandboxed_args(["ao", "send", session, prompt_text, "--timeout", "900"])
        if send_args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        proc = subprocess.run(
            send_args,
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=960,
            check=False,
            env=_sanitized_env(),
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
            )
        activity = _ao_wait_idle(session, ctx.workdir, timeout=900, project=project)
        outcome = "success" if activity in ("exited", "ready") else "failure"
        wall_ms = int((time.monotonic() - _start_ts) * 1000)
        metrics = _codergen_metrics(proc.stdout, proc.stderr, wall_ms)
        meta = {"session": session, "activity": activity}
        meta.update({k: ("" if v is None else str(v)) for k, v in metrics.items()})
        return Result(
            outcome=outcome,
            output=f"ao send session={session} activity={activity}",
            metadata=meta,
        )

    if backend == "claude":
        args = _sandboxed_args([_get_claude_executable(), "--print", "--dangerously-skip-permissions", "--setting-sources", "", prompt_text])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        try:
            proc = subprocess.run(
                args,
                cwd=ctx.workdir,
                capture_output=True,
                text=True,
                timeout=1800,  # 30 min timeout for complex tasks
                check=False,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
                env=_sanitized_env(),
            )
        except subprocess.TimeoutExpired:
            return Result(outcome="failure", output="claude backend timed out after 30 minutes")
        except Exception as e:
            return Result(outcome="failure", output=f"claude backend error: {e}")
    elif backend == "codex":
        args = _sandboxed_args(["codex", "exec", "--yolo", "--skip-git-repo-check", prompt_text])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        timeout_s = int(node.attrs.get("timeout", "1800"))
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
    elif backend == "claudew":
        wafer_key = os.environ.get("WAFER_API_KEY", "")
        if not wafer_key:
            return Result(outcome="failure", output="claudew backend requires WAFER_API_KEY env var")
        wafer_model = os.environ.get("WAFER_MODEL", "GLM-5.1")
        env = _sanitized_env()
        env["WAFER_API_KEY"] = wafer_key
        env["WAFER_MODEL"] = wafer_model
        env["ANTHROPIC_BASE_URL"] = "http://localhost:9001"
        env["ANTHROPIC_AUTH_TOKEN"] = wafer_key
        env["ANTHROPIC_DEFAULT_MODEL"] = wafer_model
        env["ANTHROPIC_DEFAULT_OPUS_MODEL"] = wafer_model
        env["ANTHROPIC_DEFAULT_SONNET_MODEL"] = wafer_model
        env["ANTHROPIC_DEFAULT_HAIKU_MODEL"] = wafer_model
        env["CLAUDEW_MODE"] = "1"
        claude_bin = _get_claude_executable()
        args = _sandboxed_args([claude_bin, "--print", "--dangerously-skip-permissions", "--model", wafer_model, "--effort", "high", prompt_text])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        try:
            proc = subprocess.run(
                args,
                cwd=ctx.workdir,
                capture_output=True,
                text=True,
                timeout=1800,
                check=False,
                stdin=subprocess.DEVNULL,
                start_new_session=True,
                env=env,
            )
        except subprocess.TimeoutExpired:
            return Result(outcome="failure", output="claudew backend timed out after 30 minutes")
        except Exception as e:
            return Result(outcome="failure", output=f"claudew backend error: {e}")
    elif backend == "agy":
        timeout_s = int(node.attrs.get("timeout", "600"))
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
    try:
        timeout = int(node.attrs.get("timeout", "300"))
    except (TypeError, ValueError):
        timeout = 300
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
    proc = subprocess.run(
        args,
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
        env=_sanitized_env(),
    )
    outcome = "success" if proc.returncode == 0 else "failure"
    return Result(
        outcome=outcome,
        output=proc.stdout + ("\nSTDERR:\n" + proc.stderr if proc.stderr else ""),
        metadata={"returncode": str(proc.returncode)},
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


_VERDICT_NORMALIZE = {
    "pass": "success",
    "warn": "success",
    "fail": "failure",
    "partial": "failure",
    "inconclusive": "failure",
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

# Anchored regex: keyword must follow a marker like "verdict:", "overall:", or "normalized:"
# and stand on its own word boundary. Avoids substring hits inside "passes warnings".
_MARKER_RE = re.compile(
    r"(?:verdict|overall|normalized)\s*:\s*(pass|warn|fail|partial|inconclusive)\b",
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
    r"^\s*(pass|warn|fail|partial|inconclusive)\b[\s.!:]*$",
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


def _slash_gate(slash_command: str, default_args: str = "") -> Handler:
    """Build a handler that shells out to `claude --print /<command> <args>`."""

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

        try:
            timeout = int(node.attrs.get("timeout", "1200"))
        except (TypeError, ValueError):
            timeout = 1200

        # Backend-aware executable selection
        if ctx.backend == "claudew":
            claude_bin = _get_claude_executable()
            wafer_model = os.environ.get("WAFER_MODEL", "GLM-5.1")
            gate_env = _sanitized_env()
            gate_env["ANTHROPIC_BASE_URL"] = "http://localhost:9001"
            gate_env["ANTHROPIC_AUTH_TOKEN"] = os.environ.get("WAFER_API_KEY", "")
            gate_env["ANTHROPIC_DEFAULT_MODEL"] = wafer_model
            gate_env["ANTHROPIC_DEFAULT_OPUS_MODEL"] = wafer_model
            gate_env["ANTHROPIC_DEFAULT_SONNET_MODEL"] = wafer_model
            gate_env["ANTHROPIC_DEFAULT_HAIKU_MODEL"] = wafer_model
            gate_env["CLAUDEW_MODE"] = "1"
            sub_args = _sandboxed_args([claude_bin, "--print", "--dangerously-skip-permissions", "--model", wafer_model, "--effort", "high", prompt])
        else:
            claude_bin = _get_claude_executable()
            gate_env = _sanitized_env()
            sub_args = _sandboxed_args([claude_bin, "--print", "--dangerously-skip-permissions", prompt])

        if sub_args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        proc = subprocess.run(
            sub_args,
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            env=gate_env,
        )
        combined = proc.stdout + "\n" + proc.stderr
        verdict, normalized = _parse_verdict(combined)
        # SHA binding check — must come BEFORE collapsing to pass/fail so a
        # spoofed-pass-with-wrong-SHA collapses to `error`, not `success`.
        sha_ok, observed_sha = _verify_head_sha_echo(combined, expected_sha)
        # Distinguish infra failures (claude crashed / not installed / network)
        # from real "FAIL" verdicts so the Healer can group them separately.
        if proc.returncode != 0 and (verdict == "unknown" or normalized == "success"):
            outcome = "error"
        elif not sha_ok:
            # SHA missing/mismatched.  A spoofed PASS is dangerous so we
            # collapse that to error.  But FAIL/PARTIAL without a SHA echo is
            # still conservative — keep the real verdict rather than hiding it.
            if normalized == "success":
                outcome = "error"
            elif normalized == "unknown":
                outcome = "error"
            else:
                outcome = normalized  # fail/partial — conservative, keep verdict
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
                "slash_command": slash_command,
                "verdict": verdict,
                "returncode": str(proc.returncode),
                "expected_head_sha": expected_sha,
                "observed_head_sha": observed_sha,
                "head_sha_status": head_sha_status,
            },
        )

    handler.__name__ = f"_gate_{slash_command}"  # noqa: WPS125
    return handler


_gate_es = _slash_gate("es")
_gate_er = _slash_gate("er")
_gate_code_standards = _slash_gate("code_standards")


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

    # Build eval env: inherit current environment...
    eval_env = dict(os.environ)

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
        # Poll until all required emulator ports respond or startup_delay expires.
        _emulator_ports = [8080, 9099, 5001]
        _deadline = time.monotonic() + startup_delay
        while time.monotonic() < _deadline:
            if all(_tcp_port_open("localhost", p) for p in _emulator_ports):
                break
            time.sleep(2)

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
    try:
        p.relative_to(root)
    except ValueError:
        return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"
    if not p.exists():
        return f"# {node.name}\n\nGoal: {ctx.goal}\n(missing prompt: {ref})"
    text = p.read_text()
    text = text.replace("${goal}", ctx.goal)
    for k, v in ctx.state.items():
        text = text.replace("${state." + k + "}", v)
    return text


REGISTRY: dict[str, Handler] = {
    # by shape
    "Mdiamond": _start,
    "Msquare": _exit,
    "hexagon": _conditional,
    # by explicit type attribute (overrides shape)
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
