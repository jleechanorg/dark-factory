"""Node handlers — what each node *does* when the engine visits it.

Handlers are looked up by node shape. Each handler returns a Result describing
the outcome, which the engine uses to pick the next edge.
"""

from __future__ import annotations

import json
import os
import pathlib
import shlex
import subprocess
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
    return Result(outcome="success", output="exit")


def _codergen(node: Node, ctx: Context) -> Result:
    """Run an LLM coding step.

    Reads the prompt template referenced by `prompt="@path"` (relative to the
    runner workdir), substitutes `${goal}` and `${state.<key>}` placeholders,
    and dispatches to the configured backend.

    Backends:
      - echo: no LLM — just record the rendered prompt. Used in tests.
      - claude: shell out to `claude --print` with --dangerously-skip-permissions
      - codex: shell out to `codex exec --yolo`
    """
    prompt_text = _render_prompt(node, ctx)
    if ctx.backend == "echo":
        return Result(outcome="success", output=prompt_text)

    if ctx.backend == "claude":
        proc = subprocess.run(
            ["claude", "--print", "--dangerously-skip-permissions", prompt_text],
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=600,
            check=False,
        )
    elif ctx.backend == "codex":
        proc = subprocess.run(
            ["codex", "exec", "--yolo", "--skip-git-repo-check", prompt_text],
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=600,
            check=False,
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


def _tool(node: Node, ctx: Context) -> Result:
    """Shell out to a deterministic command supplied via `command="..."`."""
    cmd = node.attrs.get("command")
    if not cmd:
        return Result(outcome="failure", output="no command attribute")
    proc = subprocess.run(
        shlex.split(cmd),
        cwd=ctx.workdir,
        capture_output=True,
        text=True,
        timeout=int(node.attrs.get("timeout", "300")),
        check=False,
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
    print(f"  edges: {[e.attrs.get('label', e.dst) for e in []]}")
    answer = input("approve? (success/failure): ").strip().lower() or "success"
    return Result(outcome=answer, output=f"human={answer}")


_VERDICT_PATTERNS = [
    ("fail", "failure"),
    ("partial", "failure"),
    ("inconclusive", "failure"),
    ("warn", "success"),
    ("pass", "success"),
]


def _parse_verdict(text: str) -> tuple[str, str]:
    """Extract a normalized verdict from gate output.

    Looks for `VERDICT:` or `Overall:` lines (case-insensitive) and returns
    (raw_verdict, normalized_outcome). Falls back to scanning the last 40 lines
    for verdict keywords.
    """
    lines = text.splitlines()
    candidates = []
    for line in lines:
        low = line.lower()
        if "verdict:" in low or "overall:" in low or low.strip().startswith("normalized:"):
            candidates.append(low)
    if not candidates:
        candidates = [line.lower() for line in lines[-40:]]
    for verdict, normalized in _VERDICT_PATTERNS:
        for line in candidates:
            if verdict in line:
                return verdict, normalized
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

        proc = subprocess.run(
            ["claude", "--print", "--dangerously-skip-permissions", prompt],
            cwd=ctx.workdir,
            capture_output=True,
            text=True,
            timeout=int(node.attrs.get("timeout", "1200")),
            check=False,
        )
        verdict, normalized = _parse_verdict(proc.stdout + "\n" + proc.stderr)
        return Result(
            outcome=normalized,
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
    `holdouts_repo="..."` (or env var DARK_FACTORY_HOLDOUTS). The agent never
    sees the scenarios — only the verdict.
    """
    repo = node.attrs.get("holdouts_repo") or os.environ.get(
        "DARK_FACTORY_HOLDOUTS", str(pathlib.Path.home() / "projects" / "dark-factory-holdouts")
    )
    repo_path = pathlib.Path(repo).expanduser()
    feature = node.attrs.get("feature") or ctx.state.get("feature")
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
    outcome = "success" if verdict == "pass" else verdict
    return Result(outcome=outcome, output=proc.stdout, metadata={"verdict": verdict})


def _render_prompt(node: Node, ctx: Context) -> str:
    ref = node.prompt_ref
    if not ref:
        return f"# {node.name}\n\nGoal: {ctx.goal}"
    p = ctx.workdir / ref
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
