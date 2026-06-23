"""Net-LOC + slash + pytest gates.

Rationale (jleechan-arr): the ``_gate_slash`` call site uses the
**1200s** default when a node omits the ``timeout`` attribute. The
roadmap at ``docs/plans/factory_improvement_analysis.md`` proposes
**300s** for "review / deep auditing". Observed production gate wall
clocks (gate_es / gate_er / gate_code_standards across the CXDB event
log) show p99 = 206s (max observed = 206s, never timed out). The 1200s
default intentionally leaves headroom for very large diffs that would
exceed the 300s roadmap value. See ``TIMEOUT_DEFAULTS_RATIONALE`` in
``docs/plans/factory_improvement_analysis.implementation.md`` Pillar 4
for the empirical distribution and the citation.

Owns:
  * `_resolve_base_sha` — state → attr → ``git merge-base origin/main HEAD`` →
    ``"origin/main"``.
  * `_gate_net_loc` — ``git diff --numstat`` and assert additions ≤ deletions.
  * `_gate_slash` — generic ``/<command>`` gate: materialize user-scope cmd,
    inline into prompt, route via ``_execute_gate``.
  * `_run_pytest_test` — run pytest against ``test_path`` attr (with state
    substitution).
  * `_gate_red` — RED: bug-fix bug-reproduction gate (test MUST fail).
  * `_gate_green` — GREEN: bug-fix verification gate (test MUST pass).
"""

from __future__ import annotations

import pathlib
import shlex
import subprocess
import sys
from typing import TYPE_CHECKING

import runner.handlers as _handlers_shim

from .handler_core import Result, _gate_strict_flag

if TYPE_CHECKING:
    from .parser import Node
    from .handler_core import Context


def _resolve_base_sha(node: "Node", ctx: "Context") -> str:
    base = ctx.state.get("base_sha")
    if not base:
        base = node.attrs.get("base_sha")
    if not base:
        try:
            proc = subprocess.run(
                ["git", "-C", str(ctx.workdir), "merge-base", "origin/main", "HEAD"],
                capture_output=True, text=True, timeout=15, check=False,
            )
            if proc.returncode == 0 and proc.stdout.strip():
                base = proc.stdout.strip()
        except Exception:
            pass
    if not base:
        base = "origin/main"
    return str(base)


def _gate_net_loc(node: "Node", ctx: "Context") -> "Result":
    base_sha = _resolve_base_sha(node, ctx)
    try:
        proc = subprocess.run(
            ["git", "-C", str(ctx.workdir), "diff", "--numstat", base_sha],
            capture_output=True, text=True, timeout=15, check=False,
        )
    except Exception as e:
        return Result(outcome="error", output=f"Failed to run git diff: {e}")

    if proc.returncode != 0:
        return Result(outcome="failure", output=f"git diff --numstat failed (rc={proc.returncode}):\n{proc.stdout}\n{proc.stderr}")

    total_additions = 0
    total_deletions = 0
    lines = proc.stdout.strip().splitlines()
    for line in lines:
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 2:
            add_str, del_str = parts[0], parts[1]
            try:
                total_additions += int(add_str)
            except ValueError:
                pass
            try:
                total_deletions += int(del_str)
            except ValueError:
                pass

    net_loc = total_additions - total_deletions
    output = f"Base SHA: {base_sha}\nTotal Additions: {total_additions}\nTotal Deletions: {total_deletions}\nNet LOC: {net_loc}"
    if total_additions > total_deletions:
        return Result(outcome="failure", output=f"Net LOC is positive: {output}")
    else:
        return Result(outcome="success", output=f"Net LOC is non-positive: {output}")


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
    import shutil
    command = str(node.attrs.get("command", "")).strip().lstrip("/")
    if not command:
        return Result(
            outcome="error",
            output=f"gate_slash node {node.name!r} missing required command attr",
            metadata={"verdict": "unknown", "slash_command": ""},
        )
    if _handlers_shim._node_prompt_ref(node):
        return _handlers_shim._run_custom_prompt_gate(node, ctx, f"gate_{command}")
    # Echo/mock backends never shell out, so the command-file requirement
    # doesn't apply — let the echo branch in _slash_gate seed the outcome.
    if ctx.backend in ("echo", "mock_llm"):
        return _handlers_shim._slash_gate(command)(node, ctx)
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

    # Inline the command file's content instead of sending a literal
    # "/<command>" prompt. Each reviewer CLI resolves slash commands from a
    # different namespace (claude: project + user commands; codex:
    # ~/.codex/prompts), so "/cmd" prompts are backend-dependent — observed
    # live as codex/claude "Unknown command: /zfc" lane failures. Inlined
    # instructions are universal across backends.
    expected_sha = _handlers_shim._worktree_head_sha(ctx.workdir)
    if expected_sha is None:
        return Result(
            outcome="error",
            output=f"gate /{command} cannot resolve HEAD SHA for {ctx.workdir}",
            metadata={"slash_command": command, "verdict": "unknown",
                      "head_sha_status": "missing"},
        )
    try:
        cmd_text = local_cmd.read_text()
    except OSError as exc:
        return Result(
            outcome="error",
            output=f"gate_slash: cannot read {local_cmd}: {exc}",
            metadata={"verdict": "unknown", "slash_command": command},
        )
    target = node.attrs.get("target", str(ctx.workdir))
    prompt = (
        f"You are the /{command} review lane, running as a READ-ONLY reviewer "
        f"gate. Do not modify any files.\n\n"
        f"Target under review: {target}\n\n"
        f"--- /{command} instructions ---\n{cmd_text}\n--- end instructions ---\n"
        f"\n<!-- RUNNER BINDING REQUIREMENT (non-negotiable) -->\n"
        f"expected_head_sha: {expected_sha}\n\n"
        f"CRITICAL: Your response MUST include BOTH of the following lines "
        f"verbatim (machine-parsed; do not omit, paraphrase, or put inside a "
        f"code block):\n"
        f"head_sha: {expected_sha}\n"
        f"verdict: <pass|warn|fail|partial>\n"
    )
    # Rationale (jleechan-arr): 1200s default — see module docstring for
    # the empirical basis (observed p99 = 206s, max = 206s).
    timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "1200"), 1200)
    backend, gate_meta = _handlers_shim._resolve_gate_backend(node, ctx)
    result = _handlers_shim._execute_gate(
        prompt, expected_sha, timeout, ctx, command, backend,
        gate_strict=_gate_strict_flag(node),
    )
    if gate_meta:
        for k, v in gate_meta.items():
            result.metadata.setdefault(k, v)
    return result


def _run_pytest_test(node: "Node", ctx: "Context", *, label: str) -> "Result":
    """Run pytest against the test path stored in node attrs or state.

    Returns a Result with ``outcome=success`` only if pytest exits 0.
    The caller maps 0/non-0 to its own semantics (red vs green).
    """
    raw_path = node.attrs.get("test_path", "${state.bug_fix.test_path}")
    test_path = _handlers_shim._substitute_state(str(raw_path), ctx)
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
    timeout = _handlers_shim._coerce_timeout(node.attrs.get("timeout", "300"), 300)
    args = _handlers_shim._sandboxed_args(args_list)
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
            env=_handlers_shim._sanitized_env(),
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
