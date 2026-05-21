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
import subprocess
import time
from dataclasses import dataclass, field
from typing import Callable, Optional

from .parser import Node


@dataclass
class Result:
    outcome: str = "success"  # used by edge `condition="outcome=success"`
    output: str = ""
    metadata: dict[str, str] = field(default_factory=dict)


@dataclass
class Context:
    """Mutable run state passed to every handler."""

    goal: str
    workdir: pathlib.Path
    state: dict[str, str] = field(default_factory=dict)
    history: list[dict[str, str]] = field(default_factory=list)
    backend: str = "echo"  # echo | claude | codex | shell
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
) -> str:
    """Poll `ao status --json` until the session is idle for `stable_reads`
    consecutive polls.

    During retry loops inside the agent (e.g. claude rate-limit backoff), a
    session can momentarily report "ready" between retry attempts before
    bouncing back to "active". Requiring N consecutive idle reads makes the
    wait robust against that.

    Returns the last observed terminal activity ("exited", "ready",
    "missing"), or "timeout" if the deadline elapsed before idle stabilised.
    """
    deadline = time.monotonic() + timeout
    consecutive = 0
    last_terminal = "unknown"
    while time.monotonic() < deadline:
        proc = subprocess.run(
            ["ao", "status", "--json"],
            cwd=workdir,
            capture_output=True,
            text=True,
            timeout=60,
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
      - ao: dispatch to an Agent Orchestrator worker. First call spawns a
        session (`ao spawn`); subsequent calls reuse it (`ao send`). The
        worker writes inside its own AO-managed worktree; the path is stored
        in `ctx.state["ao.worktree"]` so downstream tool nodes can target it.
    """
    prompt_text = _render_prompt(node, ctx)
    if ctx.backend == "echo":
        return Result(outcome="success", output=prompt_text)

    if ctx.backend == "ao":
        project = ctx.state.get("ao.project")
        if not project:
            return Result(outcome="failure", output="ao backend requires --ao-project")
        agent = ctx.state.get("ao.agent", "claude-code")
        session = ctx.state.get("ao.session")
        if not session:
            spawn_args = ["ao", "spawn", prompt_text, "-p", project, "--agent", agent]
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
            activity = _ao_wait_idle(sess_name, ctx.workdir, timeout=900)
            outcome = "success" if activity in ("exited", "ready") else "failure"
            return Result(
                outcome=outcome,
                output=f"ao spawn session={sess_name} worktree={worktree} activity={activity}",
                metadata={"session": sess_name, "worktree": worktree or "", "activity": activity},
            )

        send_args = ["ao", "send", session, prompt_text, "--timeout", "900"]
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
            return Result(
                outcome="failure",
                output=f"ao send failed (rc={proc.returncode})\n{proc.stdout}\nSTDERR:\n{proc.stderr}",
            )
        activity = _ao_wait_idle(session, ctx.workdir, timeout=900)
        outcome = "success" if activity in ("exited", "ready") else "failure"
        return Result(
            outcome=outcome,
            output=f"ao send session={session} activity={activity}",
            metadata={"session": session, "activity": activity},
        )

    if ctx.backend == "claude":
        args = _sandboxed_args(["claude", "--print", "--dangerously-skip-permissions", prompt_text])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        proc = subprocess.run(
            args,
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=600,
            check=False,
            env=_sanitized_env(),
        )
    elif ctx.backend == "codex":
        args = _sandboxed_args(["codex", "exec", "--yolo", "--skip-git-repo-check", prompt_text])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        proc = subprocess.run(
            args,
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=600,
            check=False,
            env=_sanitized_env(),
        )
    else:
        return Result(outcome="failure", output=f"unknown backend {ctx.backend!r}")

    outcome = "success" if proc.returncode == 0 else "failure"
    return Result(
        outcome=outcome,
        output=proc.stdout + ("\nSTDERR:\n" + proc.stderr if proc.stderr else ""),
        metadata={"returncode": str(proc.returncode)},
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
            cwd = pathlib.Path(cwd_attr).expanduser()
            if not cwd.is_absolute():
                cwd = (ctx.workdir / cwd).resolve()
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


_VERDICT_NORMALIZE = {
    "pass": "success",
    "warn": "success",
    "fail": "failure",
    "partial": "failure",
    "inconclusive": "failure",
}

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
        prompt = f"/{slash_command} {args} {target}".strip()

        # Echo backend: outcome from state hint, used by tests + CI.
        if ctx.backend == "echo":
            hint = ctx.state.get(f"{node.name}.outcome", "success")
            return Result(
                outcome=hint,
                output=f"echo gate /{slash_command}: pre-seeded {hint}",
                metadata={"slash_command": slash_command, "verdict": "echo:" + hint},
            )

        try:
            timeout = int(node.attrs.get("timeout", "1200"))
        except (TypeError, ValueError):
            timeout = 1200
        args = _sandboxed_args(["claude", "--print", "--dangerously-skip-permissions", prompt])
        if args is None:
            return Result(outcome="failure", output="sandbox-exec unavailable")
        proc = subprocess.run(
            args,
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            env=_sanitized_env(),
        )
        verdict, normalized = _parse_verdict(proc.stdout + "\n" + proc.stderr)
        # Distinguish infra failures (claude crashed / not installed / network)
        # from real "FAIL" verdicts so the Healer can group them separately.
        if proc.returncode != 0 and (verdict == "unknown" or normalized == "success"):
            outcome = "error"
        else:
            outcome = normalized
        return Result(
            outcome=outcome,
            output=proc.stdout,
            metadata={
                "slash_command": slash_command,
                "verdict": verdict,
                "returncode": str(proc.returncode),
            },
        )

    handler.__name__ = f"_gate_{slash_command}"  # noqa: WPS125
    return handler


_gate_es = _slash_gate("es")
_gate_er = _slash_gate("er")
_gate_code_standards = _slash_gate("code_standards")


def _holdout_eval(node: Node, ctx: Context) -> Result:
    """Run the sealed holdout evaluator in a separate process.

    The evaluator script lives outside this repo at the path given by
    DARK_FACTORY_HOLDOUTS or the default sibling path. Pipeline DOT cannot
    override this path; otherwise an agent-visible pipeline could fake PASS.
    """
    repo_path = _holdouts_repo_path()
    feature = ctx.state.get("feature") or node.attrs.get("feature")
    if not feature:
        return Result(outcome="failure", output="no feature attribute or state")

    eval_script = repo_path / "evaluator" / "run.py"
    if not eval_script.exists():
        return Result(
            outcome="failure",
            output=f"holdout evaluator missing: {eval_script}",
        )

    proc = subprocess.run(
        ["python3", str(eval_script), "--feature", feature, "--implementation", str(ctx.workdir)],
        cwd=repo_path,
        capture_output=True,
        text=True,
        timeout=600,
        check=False,
    )
    # The evaluator emits a final JSON line with {verdict, scenarios:[{name, status}]}.
    verdict = "failure"
    for line in proc.stdout.splitlines()[::-1]:
        line = line.strip()
        if line.startswith("{") and line.endswith("}"):
            try:
                data = json.loads(line)
                verdict = data.get("verdict", "failure").lower()
                break
            except json.JSONDecodeError:
                continue
    if proc.returncode != 0 and verdict == "pass":
        outcome = "error"
    else:
        outcome = "success" if verdict == "pass" else verdict
    return Result(outcome=outcome, output=proc.stdout, metadata={"verdict": verdict})


def _render_prompt(node: Node, ctx: Context) -> str:
    ref = node.prompt_ref
    if not ref:
        return f"# {node.name}\n\nGoal: {ctx.goal}"
    ref_path = pathlib.Path(ref)
    if ref_path.is_absolute():
        return f"# {node.name}\n\nGoal: {ctx.goal}\n(invalid prompt: {ref})"
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
    if node.name == "start":
        return _start
    if node.name == "exit":
        return _exit
    s = node.shape
    if s in REGISTRY:
        return REGISTRY[s]
    return _codergen
