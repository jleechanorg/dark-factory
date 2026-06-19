"""Tool + human-gate handlers.

Owns:
  * `_tool` — shell out to a deterministic ``command="..."`` with cwd +
    timeout, supporting ``${state.<key>}`` substitution in both.
  * `_human_gate` — pre-seed ``ctx.state["<node>.outcome"]``; else stdin read.

Both look up monkeypatched helpers (``_sandboxed_args``, ``_sanitized_env``,
``_coerce_timeout``, ``_substitute_state``, ``_path_attr``) via the
``runner.handlers`` shim (late binding).
"""

from __future__ import annotations

import os
import shlex
import subprocess
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _tool(node: "Node", ctx: "Context") -> "Result":
    """Shell out to a deterministic command supplied via `command="..."`.

    Supports `${state.<key>}` substitution in both `command` and the optional
    `cwd` attribute. `cwd` lets the node target a directory other than
    `ctx.workdir` (e.g. an AO worker's worktree path stashed in state).
    """
    cmd = node.attrs.get("command")
    if not cmd:
        return Result(outcome="failure", output="no command attribute")
    cmd = _handlers_shim._substitute_state(cmd, ctx)
    timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "300"), 300)
    cwd_attr = node.attrs.get("cwd")
    if cwd_attr:
        cwd_attr = _handlers_shim._substitute_state(cwd_attr, ctx)
        if "${state." in cwd_attr:
            # Unresolved placeholder — backend didn't set the state key.
            # Fall back to ctx.workdir so the pipeline still works under
            # backends that don't populate it (e.g. echo / claude).
            cwd = ctx.workdir
        else:
            cwd = _handlers_shim._path_attr(node, ctx, "cwd", ctx.workdir)
            if not cwd.exists():
                return Result(outcome="failure", output=f"cwd does not exist: {cwd}")
    else:
        cwd = ctx.workdir
    args = _handlers_shim._sandboxed_args(shlex.split(cmd))
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
            env=_handlers_shim._sanitized_env(),
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


def _human_gate(node: "Node", ctx: "Context") -> "Result":
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
