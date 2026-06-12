"""Tests for repo/branch performance logging under ~/Library/Logs/dark-factory (default)."""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.engine import run
from runner.handlers import Context, Result, TYPE_REGISTRY
from runner.parser import parse
from runner import perf_log


def _pipeline(name: str) -> pathlib.Path:
    return ROOT / "pipelines" / "factory" / name


def test_slugify_branch_with_slashes():
    assert perf_log._safe_slug("feat/my-feature") == "feat_my-feature"


def test_parse_repo_name_from_scp_style():
    assert perf_log._parse_repo_name("git@github.com:org/worldarchitect.ai.git") == "worldarchitect.ai"


def test_git_context_from_workdir(monkeypatch, tmp_path):
    def fake_git(workdir: pathlib.Path, *args: str):
        cmd = list(args)
        if cmd == ["rev-parse", "--abbrev-ref", "HEAD"]:
            return "feat/my-branch"
        if cmd == ["remote", "get-url", "origin"]:
            return "https://github.com/jleechanorg/dark-factory.git"
        if cmd == ["rev-parse", "HEAD"]:
            return "a" * 40
        return None

    monkeypatch.setattr(perf_log, "_git_cmd", fake_git)
    ctx = perf_log.resolve_git_context(tmp_path)
    assert ctx.repo_slug == "dark-factory"
    assert ctx.branch_slug == "feat_my-branch"
    assert ctx.head_sha == "a" * 40


def test_git_context_fallback_without_git(monkeypatch, tmp_path):
    monkeypatch.setattr(perf_log, "_git_cmd", lambda *_a, **_k: None)
    ctx = perf_log.resolve_git_context(tmp_path)
    assert ctx.repo_slug == tmp_path.name
    assert ctx.branch_slug == "unknown"
    assert ctx.head_sha is None


def _read_jsonl(path: pathlib.Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def test_perf_log_writes_enter_exit_success(monkeypatch, tmp_path):
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="fake pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    monkeypatch.setattr(
        perf_log,
        "_git_cmd",
        lambda *_a, **_k: "main" if len(_a) > 1 and _a[1] == "rev-parse" else None,
    )

    graph = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="perf test",
        workdir=ROOT,
        backend="echo",
        perf_log_root=tmp_path / "perf",
    )
    history = run(graph, ctx, max_steps=50)

    assert history[-1].outcome == "success"
    assert ctx.perf_run is not None
    assert ctx.perf_run.jsonl_path.exists()
    assert ctx.perf_run.log_path.exists()

    events = _read_jsonl(ctx.perf_run.jsonl_path)
    event_names = [e["event"] for e in events]
    assert "run_start" in event_names
    assert "node_enter" in event_names
    assert "node_exit" in event_names
    assert "run_end" in event_names

    enter_events = [e for e in events if e["event"] == "node_enter"]
    exit_events = [e for e in events if e["event"] == "node_exit"]
    assert len(enter_events) == len(exit_events)

    log_text = ctx.perf_run.log_path.read_text()
    assert "ENTER node=" in log_text
    assert "EXIT node=" in log_text
    assert "RUN_START" in log_text
    assert "RUN_END" in log_text

    index_path = ctx.perf_run.run_dir / "runs.index.jsonl"
    assert index_path.exists()
    index_rows = _read_jsonl(index_path)
    assert index_rows[-1]["final_outcome"] == "success"


def test_perf_log_records_failure_outcome(monkeypatch, tmp_path):
    def fake_holdout(node, ctx):
        return Result(outcome="fail", output="fake fail")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    graph = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="fail test",
        workdir=ROOT,
        backend="echo",
        perf_log_root=tmp_path / "perf",
    )
    history = run(graph, ctx, max_steps=50)

    assert history[-1].outcome == "exhausted"
    events = _read_jsonl(ctx.perf_run.jsonl_path)
    holdout_exits = [
        e for e in events if e["event"] == "node_exit" and e.get("node") == "holdout"
    ]
    assert holdout_exits
    assert holdout_exits[0]["outcome"] == "failure"
    assert holdout_exits[0]["success"] == "false"


def test_perf_log_records_error_on_exception(monkeypatch, tmp_path):
    def boom(node, ctx):
        raise RuntimeError("backend exploded")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", boom)
    graph = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="error test",
        workdir=ROOT,
        backend="echo",
        perf_log_root=tmp_path / "perf",
    )
    history = run(graph, ctx, max_steps=50)

    assert any(step.outcome == "error" for step in history)
    events = _read_jsonl(ctx.perf_run.jsonl_path)
    error_exits = [e for e in events if e["event"] == "node_exit" and e.get("outcome") == "error"]
    assert error_exits


def test_no_perf_log_when_disabled(monkeypatch, tmp_path):
    def fake_holdout(node, ctx):
        return Result(outcome="success", output="fake pass")

    monkeypatch.setitem(TYPE_REGISTRY, "holdout_eval", fake_holdout)
    graph = parse(_pipeline("hello.dot"))
    ctx = Context(
        goal="disabled",
        workdir=ROOT,
        backend="echo",
        perf_log_root=None,
    )
    run(graph, ctx, max_steps=50)
    assert ctx.perf_run is None
    perf_dir = tmp_path / "perf"
    assert not perf_dir.exists() or not any(perf_dir.rglob("*"))


def test_parallel_branch_enter_exit(monkeypatch, tmp_path):
    dot = tmp_path / "parallel.dot"
    dot.write_text(
        'digraph parallel {\n'
        '  graph [goal="Parallel perf test" rankdir=LR]\n'
        '  start [shape=Mdiamond, label="Start"]\n'
        '  fan [type="codergen", label="Fanout", parallel="true", allow_partial="true", join_quorum=2]\n'
        '  branch_pass [type="branch_pass", label="Branch pass"]\n'
        '  branch_partial [type="branch_partial", label="Branch partial"]\n'
        '  exit [shape=Msquare, label="Exit"]\n'
        '  start -> fan\n'
        '  fan -> branch_pass\n'
        '  fan -> branch_partial\n'
        '  fan -> exit [join=true]\n'
        '}\n'
    )
    monkeypatch.setitem(TYPE_REGISTRY, "branch_pass", lambda node, ctx: Result(outcome="success", output="ok"))
    monkeypatch.setitem(TYPE_REGISTRY, "branch_partial", lambda node, ctx: Result(outcome="partial", output="ok"))

    graph = parse(dot)
    ctx = Context(
        goal="parallel",
        workdir=ROOT,
        backend="echo",
        perf_log_root=tmp_path / "perf",
    )
    run(graph, ctx, max_steps=20)

    events = _read_jsonl(ctx.perf_run.jsonl_path)
    branch_enters = [
        e for e in events
        if e["event"] == "node_enter" and e.get("node") in {"branch_pass", "branch_partial"}
    ]
    branch_exits = [
        e for e in events
        if e["event"] == "node_exit" and e.get("node") in {"branch_pass", "branch_partial"}
    ]
    assert len(branch_enters) == 2
    assert len(branch_exits) == 2


def test_cli_no_perf_log_flag(tmp_path):
    dot = tmp_path / "minimal.dot"
    dot.write_text(
        'digraph minimal {\n'
        '  graph [goal="minimal"]\n'
        '  start [shape=Mdiamond]\n'
        '  exit [shape=Msquare]\n'
        '  start -> exit\n'
        '}\n'
    )
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner",
            "--pipeline",
            str(dot),
            "--goal",
            "cli no perf",
            "--backend",
            "echo",
            "--no-perf-log",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    summary = json.loads(proc.stdout)
    assert "perf_log" not in summary


def test_cli_perf_log_in_summary(tmp_path):
    dot = tmp_path / "minimal.dot"
    dot.write_text(
        'digraph minimal {\n'
        '  graph [goal="minimal"]\n'
        '  start [shape=Mdiamond]\n'
        '  exit [shape=Msquare]\n'
        '  start -> exit\n'
        '}\n'
    )
    perf_root = tmp_path / "cli-perf"
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner",
            "--pipeline",
            str(dot),
            "--goal",
            "cli perf",
            "--backend",
            "echo",
            "--perf-log-dir",
            str(perf_root),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    summary = json.loads(proc.stdout)
    assert "perf_log" in summary
    assert summary["perf_log"]["jsonl"]
    assert summary["perf_log"]["log"]
    assert pathlib.Path(summary["perf_log"]["jsonl"]).exists()
