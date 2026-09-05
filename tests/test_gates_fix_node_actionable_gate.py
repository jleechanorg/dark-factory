"""End-to-end reproduction test for dark-factory#828.

Real incident: a "review/verify only, do not change code" run through
pipelines/factory/gates.dot hit a gate_skeptic node whose LLM backend
rendered `VERDICT: None` (a template-substitution bug, no real verdict).
The pipeline scored that as outcome=failure (indistinguishable from a real
rejection) and routed it to the `fix` node, which then had full repo
write+push authority and nothing actionable to act on — it improvised,
deleting a test suite, reverting a closed P0, and pushing to a LIVE PR
branch, TWICE, from two different backends, in one session.

This test drives the REAL pipelines/factory/gates.dot (and pr_gates.dot)
through the engine with a scripted gate_skeptic backend that reproduces the
exact null-verdict shape via the REAL `_parse_verdict` (not a hand-picked
outcome string), and asserts:
  1. `fix` never executes.
  2. Zero commits are created in the target workdir (a real git repo).
  3. The run reaches `exit`, not an indefinite stuck/exhausted loop.

RED proof (verified manually, not re-asserted here to avoid depending on
git history): checking out the pre-fix pipelines/factory/gates.dot (edges
`gate_skeptic -> fix [condition="outcome!=success"]`) with the pre-fix
`runner/handler_verdict.py` (null verdict normalized to "failure") and
running this same test fails — `fix` executes and creates a commit.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run  # noqa: E402
from runner.handlers import Context, Result, TYPE_REGISTRY  # noqa: E402
from runner.handler_verdict import _parse_verdict  # noqa: E402

# The exact incident shape from the CXDB evidence in dark-factory#827/#828:
# a rendered template with the verdict placeholder substituted with
# Python's `None`, but a correctly-echoed head_sha (so this is not a SHA
# mismatch / spoofing case — the gate genuinely ran, it just produced no
# verdict).
_NULL_VERDICT_OUTPUT = (
    "<!-- skeptic-gate-verdict -->\n"
    "## Skeptic Gate — `None`\n\n"
    "**VERDICT: None**\n"
)


def _git(*args: str, cwd: pathlib.Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args], cwd=str(cwd), capture_output=True, text=True, check=False
    )


def _init_repo(workdir: pathlib.Path) -> None:
    workdir.mkdir(parents=True, exist_ok=True)
    _git("init", "-q", cwd=workdir)
    _git("config", "user.email", "test@example.com", cwd=workdir)
    _git("config", "user.name", "Test", cwd=workdir)
    (workdir / "README.md").write_text("original\n", encoding="utf-8")
    _git("add", "-A", cwd=workdir)
    _git("commit", "-q", "-m", "initial", cwd=workdir)


def _commit_count(workdir: pathlib.Path) -> int:
    proc = _git("rev-list", "--count", "HEAD", cwd=workdir)
    return int(proc.stdout.strip() or "0")


def _run_null_verdict_repro(monkeypatch, tmp_path, pipeline_name: str):
    """Shared driver: run `pipeline_name` with a scripted null-verdict
    gate_skeptic and a fix node that would visibly mutate the repo if
    reached. Returns (history, workdir, commits_before)."""
    from runner.parser import parse

    graph = parse(ROOT / "pipelines" / "factory" / pipeline_name)
    workdir = tmp_path / "target_repo"
    _init_repo(workdir)
    commits_before = _commit_count(workdir)

    def fake_holdout(node, ctx):
        return Result(outcome="success", output="holdout ok")

    def fake_null_verdict_skeptic(node, ctx):
        # Route through the REAL verdict parser, exactly like the real
        # gate_skeptic handler does — this is the actual code path being
        # fixed, not a hand-picked outcome string. A null/unparseable
        # verdict normalizes to outcome=error (an infra state, NOT a
        # failure finding) — see runner/handler_verdict.py.
        raw, outcome = _parse_verdict(_NULL_VERDICT_OUTPUT)
        assert outcome == "error", (
            f"expected the null-verdict shape to parse as error, got {outcome!r}"
        )
        return Result(outcome=outcome, output=_NULL_VERDICT_OUTPUT, metadata={"verdict": raw})

    def destructive_fix(node, ctx):
        # If this ever runs, prove it by actually mutating the repo — the
        # same class of harm as the real incident (a commit landing with
        # nothing real to fix).
        (workdir / "DESTRUCTIVE_CHANGE.txt").write_text("should never happen\n", encoding="utf-8")
        _git("add", "-A", cwd=workdir)
        _git("commit", "-q", "-m", "improvised fix", cwd=workdir)
        return Result(outcome="success", output="fix applied")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_skeptic", fake_null_verdict_skeptic)
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", destructive_fix)

    ctx = Context(goal="Adversarially review; verify only, do not change code.", workdir=workdir, backend="echo")
    history = run(graph, ctx, max_steps=30)
    return history, workdir, commits_before


def test_gates_dot_null_verdict_never_reaches_fix(monkeypatch, tmp_path):
    history, workdir, commits_before = _run_null_verdict_repro(monkeypatch, tmp_path, "gates.dot")
    executed_nodes = [step.node for step in history]

    assert "gate_skeptic" in executed_nodes
    assert "fix" not in executed_nodes, (
        f"fix must never run on a null/inconclusive verdict; executed={executed_nodes}"
    )
    assert "exit" in executed_nodes, (
        f"run must reach exit, not get stuck; executed={executed_nodes}"
    )
    assert not (workdir / "DESTRUCTIVE_CHANGE.txt").exists()
    assert _commit_count(workdir) == commits_before, (
        "zero commits must be created in the target workdir for a review-only run"
    )


def test_pr_gates_dot_null_verdict_never_reaches_fix(monkeypatch, tmp_path):
    history, workdir, commits_before = _run_null_verdict_repro(monkeypatch, tmp_path, "pr_gates.dot")
    executed_nodes = [step.node for step in history]

    assert "fix" not in executed_nodes, (
        f"fix must never run on a null/inconclusive verdict; executed={executed_nodes}"
    )
    assert "exit" in executed_nodes
    assert not (workdir / "DESTRUCTIVE_CHANGE.txt").exists()
    assert _commit_count(workdir) == commits_before


def test_verify_dot_has_no_code_producing_node():
    """dark-factory#828 item (a): pipelines/factory/verify.dot is the
    STRUCTURAL guarantee — a review-only pipeline with no fix/coder node at
    all, so no gate outcome, verdict, or routing bug can ever cause a
    write. Unlike gates.dot (fixed at the routing-logic level above), this
    holds even if verdict-parsing regresses again."""
    from runner import graph_audit as _graph_audit
    from runner import handlers as _handlers
    from runner.parser import parse

    g = parse(ROOT / "pipelines" / "factory" / "verify.dot")
    assert "start" in g.nodes and "exit" in g.nodes
    for name, node in g.nodes.items():
        if name in ("start", "exit"):
            continue
        assert not _graph_audit._is_code_producing(node), (
            f"verify.dot node {name!r} is code-producing (resolves to the "
            f"codergen handler) — verify.dot must contain NO node capable "
            f"of writing to the target repo"
        )
    # Explicit belt-and-braces: no node literally declares type="codergen"
    # and no node is unlabeled (which would default-resolve to codergen).
    for name, node in g.nodes.items():
        if name in ("start", "exit"):
            continue
        assert node.attrs.get("type") not in (None, "", "codergen"), (
            f"verify.dot node {name!r} has no explicit non-codergen type"
        )
    violations = _graph_audit.audit_graph(ROOT / "pipelines" / "factory" / "verify.dot")
    assert violations == [], f"graph_audit violations on verify.dot: {violations}"


def test_verify_dot_null_verdict_reaches_exit_with_zero_commits(monkeypatch, tmp_path):
    """Same repro shape as test_gates_dot_null_verdict_never_reaches_fix,
    against verify.dot — proves the structural guarantee holds for the
    exact incident shape too, not just the gates.dot routing fix."""
    from runner.parser import parse

    graph = parse(ROOT / "pipelines" / "factory" / "verify.dot")
    workdir = tmp_path / "target_repo"
    _init_repo(workdir)
    commits_before = _commit_count(workdir)

    def fake_holdout(node, ctx):
        return Result(outcome="success", output="holdout ok")

    def fake_null_verdict_skeptic(node, ctx):
        raw, outcome = _parse_verdict(_NULL_VERDICT_OUTPUT)
        return Result(outcome=outcome, output=_NULL_VERDICT_OUTPUT, metadata={"verdict": raw})

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_skeptic", fake_null_verdict_skeptic)

    ctx = Context(goal="Adversarially review; verify only, do not change code.", workdir=workdir, backend="echo")
    history = run(graph, ctx, max_steps=30)
    executed_nodes = [step.node for step in history]

    assert "exit" in executed_nodes
    assert "codergen" not in executed_nodes  # no such node exists in this graph
    assert _commit_count(workdir) == commits_before


def test_gates_dot_real_failure_still_reaches_fix(monkeypatch, tmp_path):
    """Control case: a GENUINE reviewer rejection (a real finding) must
    still route to fix — this fix must not turn `fix` into dead code."""
    from runner.parser import parse

    graph = parse(ROOT / "pipelines" / "factory" / "gates.dot")
    workdir = tmp_path / "target_repo"
    _init_repo(workdir)

    def fake_holdout(node, ctx):
        return Result(outcome="success", output="holdout ok")

    def fake_real_failure_skeptic(node, ctx):
        raw, outcome = _parse_verdict("**VERDICT: FAIL** — off-by-one in the parser.")
        assert outcome == "failure"
        return Result(outcome=outcome, output="real finding", metadata={"verdict": raw})

    visited_fix = {"count": 0}

    def counting_fix(node, ctx):
        visited_fix["count"] += 1
        return Result(outcome="success", output="fixed")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    monkeypatch.setitem(TYPE_REGISTRY, "gate_skeptic", fake_real_failure_skeptic)
    monkeypatch.setitem(TYPE_REGISTRY, "codergen", counting_fix)

    ctx = Context(goal="Review this diff.", workdir=workdir, backend="echo")
    history = run(graph, ctx, max_steps=30)
    executed_nodes = [step.node for step in history]

    assert "fix" in executed_nodes, "a genuine failure finding must still route to fix"
    assert visited_fix["count"] >= 1
