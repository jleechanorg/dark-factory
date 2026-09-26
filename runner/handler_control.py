"""Tool + human-gate handlers.

Owns:
  * `_tool` — shell out to a deterministic ``command="..."`` with cwd +
    timeout, supporting ``${state.<key>}`` substitution in both.
  * `_human_gate` — pre-seed ``ctx.state["<node>.outcome"]``; else stdin read.

Both look up monkeypatched helpers (``_sandboxed_args``, `_sanitized_env`,
``_coerce_timeout``, ``_substitute_state``, ``_path_attr``) via the
``runner.handlers`` shim (late binding).
"""

from __future__ import annotations

import os
import pathlib
import shlex
import subprocess
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

from .handler_core import Result

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


# Head-truncation for the last_test_output state key. Keeps the fix prompt
# under the model's context window even when pytest prints 200KB of traceback.
_LAST_TEST_OUTPUT_MAX_CHARS = 4000


def _is_pytest_command(cmd: str) -> bool:
    """True when `cmd` looks like a pytest invocation (used to gate the
    state-vs-reality `.py` existence check). Recognizes both
    `python -m pytest ...` and `pytest ...` invocations, plus variants
    like `pytest-3` and absolute paths like `/usr/bin/pytest` or
    `~/.local/bin/pytest`.

    The check is on **tokens**, not the raw command line, so a path
    like `/tmp/pytest-of-jleechan/foo.py` does NOT trigger the
    test-path-existence pre-flight (the parent directory just happens
    to contain the substring "pytest").
    """
    def _is_pytest_token(tok: str) -> bool:
        if tok == "pytest" or tok.startswith("pytest-"):
            return True
        # Absolute or relative path: /usr/bin/pytest, ./pytest, ../bin/pytest
        base = os.path.basename(tok)
        return base == "pytest" or base.startswith("pytest-")

    try:
        tokens = shlex.split(cmd)
    except ValueError:
        # Unparseable command — fall back to a permissive check that
        # still rejects ghost `.py` paths via _extract_pytest_paths.
        return True
    for i, tok in enumerate(tokens):
        if _is_pytest_token(tok):
            return True
        # `python -m pytest` / `python3 -m pytest` — peek the next two
        if tok in {"-m", "-mp"} and i + 1 < len(tokens):
            if _is_pytest_token(tokens[i + 1]):
                return True
    return False


def _extract_pytest_paths(cmd: str) -> list[str]:
    """Return positional `.py` path tokens from a pytest command line.

    Skips flags (`-v`, `--tb=short`, etc.) by token shape. Does not
    interpret globbing; the caller checks each path with `.exists()`.
    """
    out: list[str] = []
    for tok in shlex.split(cmd):
        if tok.endswith(".py") and not tok.startswith("-"):
            out.append(tok)
    return out


def _check_test_command_paths(cmd: str, cwd: pathlib.Path) -> str | None:
    """Return an error message if any `.py` path in `cmd` does not exist
    on disk relative to `cwd`. Returns None when the command is safe to
    run. Skips the check for non-pytest commands.
    """
    if not _is_pytest_command(cmd):
        return None
    missing: list[str] = []
    for raw in _extract_pytest_paths(cmd):
        path = pathlib.Path(raw)
        if not path.is_absolute():
            path = cwd / path
        if not path.exists():
            missing.append(str(path))
    if missing:
        joined = "\n  - ".join(missing)
        return (
            f"test_command references missing file(s):\n  - {joined}\n"
            f"(from command: {cmd})\n"
            f"This usually means --state slim.test_command=... was set with a "
            f"stale filename. Update the test_command to match files that "
            f"actually exist in the worktree (or to use pytest test discovery)."
        )
    return None


def _record_test_failure_state(
    ctx: "Context",
    *,
    cmd: str,
    rc: str,
    output: str,
) -> None:
    """When a goal_gate tool node fails, stash the test output in ctx.state
    so the fix prompt can read it via `${state.last_test_output}` etc.

    Three keys are written:
      - last_test_command: the resolved command (post-substitution)
      - last_test_rc: the returncode as a string
      - last_test_output: stdout+stderr, head-truncated to
        ``_LAST_TEST_OUTPUT_MAX_CHARS`` so the fix prompt stays small

    No-op if `last_test_output` is already set, so the FIRST failure wins
    and later (possibly noisier) runs don't overwrite the canonical record.
    """
    if "last_test_output" in ctx.state:
        return
    ctx.state["last_test_command"] = cmd
    ctx.state["last_test_rc"] = rc
    head = (output or "").strip()
    if len(head) > _LAST_TEST_OUTPUT_MAX_CHARS:
        head = head[:_LAST_TEST_OUTPUT_MAX_CHARS] + "\n... [truncated]"
    ctx.state["last_test_output"] = head


def _coerce_goal_gate(node: "Node") -> bool:
    raw = node.attrs.get("goal_gate", False)
    if isinstance(raw, bool):
        return raw
    return str(raw).strip().lower() in {"true", "1", "yes"}


def _tool(node: "Node", ctx: "Context") -> "Result":
    """Shell out to a deterministic command supplied via `command="..."`.

    Supports `${state.<key>}` substitution in both `command` and the optional
    `cwd` attribute. `cwd` lets the node target a directory other than
    `ctx.workdir` (e.g. an AO worker's worktree path stashed in state).

    For pytest commands on goal_gate nodes, the resolved `.py` paths are
    pre-flighted against the filesystem so a stale --state test_command
    fails loudly with the missing filename instead of wasting LLM fix-loop
    budget on a phantom `rc=4`. For any goal_gate node that fails, the
    command + rc + head-truncated output are recorded in `ctx.state` under
    `last_test_*` keys so downstream `fix` codergen nodes can read the
    actual failure via `${state.last_test_output}` substitution.
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
    goal_gate = _coerce_goal_gate(node)
    if goal_gate:
        err = _check_test_command_paths(cmd, cwd)
        if err is not None:
            return Result(
                outcome="failure",
                output=err,
                metadata={
                    "command": cmd,
                    "returncode": "",
                    "timed_out": "false",
                    "missing_test_files": "true",
                },
            )
    if any(op in cmd for op in ("&&", "||", ";", "|", "\n")):
        cmd_args = ["/bin/bash", "-c", cmd]
    else:
        cmd_args = shlex.split(cmd)
    args = _handlers_shim._sandboxed_args(cmd_args)
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
        combined = (stdout + ("\nSTDERR:\n" + stderr if stderr else "")).strip()
        output_text = combined or f"tool command timed out after {timeout} seconds"
        if goal_gate:
            _record_test_failure_state(ctx, cmd=cmd, rc="", output=output_text)
        return Result(
            outcome="failure",
            output=output_text,
            metadata={
                "command": cmd,
                "timeout": str(timeout),
                "timed_out": "true",
                "returncode": "",
            },
        )
    except Exception as exc:
        output_text = f"tool command failed: {exc}"
        if goal_gate:
            _record_test_failure_state(ctx, cmd=cmd, rc="", output=output_text)
        return Result(
            outcome="error",
            output=output_text,
            metadata={
                "command": cmd,
                "timed_out": "false",
                "timeout": str(timeout),
                "returncode": "",
            },
        )
    outcome = "success" if proc.returncode == 0 else "failure"
    output_text = proc.stdout + ("\nSTDERR:\n" + proc.stderr if proc.stderr else "")
    if goal_gate and outcome != "success":
        _record_test_failure_state(
            ctx, cmd=cmd, rc=str(proc.returncode), output=output_text,
        )
    return Result(
        outcome=outcome,
        output=output_text,
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
