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


def _get_claude_executable() -> str:
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
            return Result(
                outcome=outcome,
                output=f"ao spawn session={sess_name} worktree={worktree} activity={activity}",
                metadata={"session": sess_name, "worktree": worktree or "", "activity": activity},
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
            return Result(
                outcome="failure",
                output=f"ao send failed (rc={proc.returncode})\n{proc.stdout}\nSTDERR:\n{proc.stderr}",
            )
        activity = _ao_wait_idle(session, ctx.workdir, timeout=900, project=project)
        outcome = "success" if activity in ("exited", "ready") else "failure"
        return Result(
            outcome=outcome,
            output=f"ao send session={session} activity={activity}",
            metadata={"session": session, "activity": activity},
        )

    if ctx.backend == "claude":
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
        if ctx.backend == "echo":
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
            f"\n\nexpected_head_sha: {expected_sha}\n"
            f"Your verdict response MUST include a line of the exact form "
            f"`head_sha: {expected_sha}` so the runner can bind this verdict "
            f"to the worktree commit it was meant to review.\n"
        )
        prompt = f"/{slash_command} {args} {target}".strip() + sha_directive

        try:
            timeout = int(node.attrs.get("timeout", "1200"))
        except (TypeError, ValueError):
            timeout = 1200
        args = _sandboxed_args([_get_claude_executable(), "--print", "--dangerously-skip-permissions", "--setting-sources", "", prompt])
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
            # Echo missing or mismatched — treat as infra error (verdict can
            # not be safely bound to a commit) regardless of pass/fail content.
            outcome = "error"
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


def _holdout_eval(node: Node, ctx: Context) -> Result:
    """Run the sealed holdout evaluator in a separate process.

    Random port allocation per run to avoid conflicts when running multiple benchmarks.
    """
    import random, time

    repo_path = _holdouts_repo_path()
    feature = ctx.state.get("feature") or node.attrs.get("feature")
    if not feature:
        return Result(outcome="failure", output="no feature attribute or state")

    eval_script = repo_path / "evaluator" / "run.py"
    if not eval_script.exists():
        return Result(outcome="failure", output=f"holdout evaluator missing: {eval_script}")

    impl_attr = node.attrs.get("implementation")
    if impl_attr:
        resolved = _substitute_state(impl_attr, ctx)
        if _has_unresolved_state_placeholder(resolved):
            return Result(outcome="failure", output=f"unresolved implementation: {impl_attr}")

    impl = _path_attr(node, ctx, "implementation", ctx.workdir)
    if not impl.exists():
        return Result(outcome="failure", output=f"implementation missing: {impl}")

    port = random.randint(30001, 30999)
    eval_env = dict(os.environ)
    eval_env["BENCHMARK_PORT"] = str(port)

    server_proc = None
    if (impl / "Makefile").exists():
        env_p = dict(eval_env)
        env_p["PORT"] = str(port)
        server_proc = subprocess.Popen(
            ["make", "run"], cwd=str(impl), env=env_p,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        time.sleep(5)

    try:
        proc = subprocess.run(
            ["python3", str(eval_script), "--feature", feature, "--impl", str(impl)],
            cwd=repo_path, capture_output=True, text=True, timeout=600, check=False, env=eval_env)
        verdict = "failure"
        for line in reversed(proc.stdout.splitlines()):
            if line.strip().startswith("{") and line.strip().endswith("}"):
                try:
                    data = json.loads(line.strip())
                    verdict = data.get("verdict", "failure").lower()
                    
                    # Write holdout results for scoring candidate
                    scenarios = data.get("scenarios", [])
                    passed = sum(1 for sc in scenarios if sc.get("status") == "pass")
                    total = len(scenarios)
                    results_file = impl / "results" / "holdout_results.json"
                    results_file.parent.mkdir(exist_ok=True)
                    results_file.write_text(json.dumps({
                        "passed": passed,
                        "total": total,
                        "scenarios": scenarios
                    }, indent=2))
                    
                    break
                except: pass
        outcome = "error" if proc.returncode and verdict == "pass" else verdict
        return Result(outcome="success" if verdict == "pass" else verdict, output=proc.stdout, metadata={"verdict": verdict, "port": str(port)})
    finally:
        if server_proc:
            server_proc.terminate()
            try: server_proc.wait(5)
            except: server_proc.kill()

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
