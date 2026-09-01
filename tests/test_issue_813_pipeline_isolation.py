"""Regression coverage for issue #813 pipeline and fresh-review preflight."""

from __future__ import annotations

import json
import os
import pathlib
import sqlite3
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner.__main__ import main
from runner.engine import run
from runner.handlers import TYPE_REGISTRY, Context, Result
from runner.parser import parse
from runner.paths import resolve_pipeline_path


def _repo(path: pathlib.Path) -> pathlib.Path:
    path.mkdir()
    subprocess.run(["git", "-C", str(path), "init", "-q", "-b", "main"], check=True)
    subprocess.run(["git", "-C", str(path), "config", "user.email", "test@example.invalid"], check=True)
    subprocess.run(["git", "-C", str(path), "config", "user.name", "test"], check=True)
    (path / "README.md").write_text("fixture\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(path), "add", "README.md"], check=True)
    subprocess.run(["git", "-C", str(path), "commit", "-qm", "baseline"], check=True)
    return path


def test_short_two_node_pipeline_name_resolves_to_factory_canonical_graph(
    tmp_path, monkeypatch
):
    factory = tmp_path / "factory"
    canonical = factory / "pipelines" / "slim" / "two_node.dot"
    canonical.parent.mkdir(parents=True)
    canonical.write_text("digraph SlimTwoNode { start -> exit }", encoding="utf-8")
    workdir = tmp_path / "target"
    workdir.mkdir()
    monkeypatch.setenv("DARK_FACTORY_HOME", str(factory))

    assert resolve_pipeline_path("two_node", workdir=workdir) == canonical.resolve()


def test_cli_preflight_resolves_short_two_node_name(tmp_path, monkeypatch):
    workdir = _repo(tmp_path / "target")
    monkeypatch.setenv("DARK_FACTORY_HOME", str(ROOT))

    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner",
            "--pipeline",
            "two_node",
            "--preflight",
            "--workdir",
            str(workdir),
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
        env={**os.environ, "DARK_FACTORY_HOME": str(ROOT)},
    )

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "pass"
    assert pathlib.Path(payload["pipeline"]) == (
        ROOT / "pipelines" / "slim" / "two_node.dot"
    ).resolve()


def test_dangling_fresh_review_skill_link_fails_before_worker_and_closes_run(
    tmp_path, monkeypatch
):
    repo = _repo(tmp_path / "target")
    skills = repo / ".codex" / "skills"
    skills.mkdir(parents=True)
    (skills / "goal-define").symlink_to("../../.claude/skills/goal-define")
    calls: list[str] = []

    def unexpected_worker(node, ctx):
        calls.append(node.name)
        return Result(outcome="success", output="must not run")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", unexpected_worker)
    events = tmp_path / "events.jsonl"
    cxdb_path = tmp_path / "run.sqlite"
    ctx = Context(
        goal="review",
        workdir=repo,
        backend="echo",
        cxdb_path=cxdb_path,
        event_log_path=events,
    )
    graph = parse(ROOT / "pipelines" / "slim" / "two_node.dot")

    history = run(graph, ctx, max_steps=10)

    assert calls == []
    assert history[-1].node == "__fresh_review_preflight__"
    assert history[-1].outcome == "error"
    assert "dangling" in history[-1].output_preview.lower()
    event_records = [json.loads(line) for line in events.read_text().splitlines()]
    assert event_records[-1]["event"] == "run_end"
    assert event_records[-1]["final_outcome"] == "error"
    with sqlite3.connect(cxdb_path) as connection:
        assert connection.execute(
            "SELECT final FROM runs WHERE run_id = ?", (ctx.run_id,)
        ).fetchone() == ("error",)
        assert connection.execute(
            "SELECT outcome FROM steps WHERE run_id = ? AND node = '__run_end__'",
            (ctx.run_id,),
        ).fetchone() == ("error",)


def test_non_git_fresh_review_target_fails_before_worker_and_closes_run(
    tmp_path, monkeypatch
):
    target = tmp_path / "not-a-repository"
    target.mkdir()
    calls: list[str] = []

    def unexpected_worker(node, ctx):
        calls.append(node.name)
        return Result(outcome="success", output="must not run")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", unexpected_worker)
    events = tmp_path / "events.jsonl"
    cxdb_path = tmp_path / "run.sqlite"
    ctx = Context(
        goal="review",
        workdir=target,
        backend="echo",
        cxdb_path=cxdb_path,
        event_log_path=events,
    )
    graph = parse(ROOT / "pipelines" / "slim" / "two_node.dot")

    history = run(graph, ctx, max_steps=10)

    assert calls == []
    assert history[-1].node == "__fresh_review_preflight__"
    assert history[-1].outcome == "error"
    assert "git repository" in history[-1].output_preview.lower()
    event_records = [json.loads(line) for line in events.read_text().splitlines()]
    assert event_records[-1]["event"] == "run_end"
    assert event_records[-1]["final_outcome"] == "error"
    with sqlite3.connect(cxdb_path) as connection:
        assert connection.execute(
            "SELECT final FROM runs WHERE run_id = ?", (ctx.run_id,)
        ).fetchone() == ("error",)
        assert connection.execute(
            "SELECT outcome FROM steps WHERE run_id = ? AND node = '__run_end__'",
            (ctx.run_id,),
        ).fetchone() == ("error",)


def test_cli_dangling_fresh_review_link_closes_before_worker(
    tmp_path, monkeypatch, capsys
):
    repo = _repo(tmp_path / "target")
    skills = repo / ".codex" / "skills"
    skills.mkdir(parents=True)
    (skills / "goal-define").symlink_to("../../.claude/skills/goal-define")
    calls: list[str] = []

    def unexpected_worker(node, ctx):
        calls.append(node.name)
        return Result(outcome="success", output="must not run")

    monkeypatch.setitem(TYPE_REGISTRY, "codergen", unexpected_worker)
    cxdb_path = tmp_path / "run.sqlite"
    events = tmp_path / "events.jsonl"
    rc = main(
        [
            "--pipeline",
            str(ROOT / "pipelines" / "slim" / "two_node.dot"),
            "--goal",
            "review",
            "--backend",
            "echo",
            "--workdir",
            str(repo),
            "--cxdb",
            str(cxdb_path),
            "--events",
            str(events),
            "--evidence-bundle",
            str(tmp_path / "evidence"),
            "--no-perf-log",
        ]
    )

    assert rc == 1
    assert calls == []
    payload = json.loads(capsys.readouterr().out)
    assert payload["final_outcome"] == "error"
    assert payload["trace"][-1]["node"] == "__fresh_review_preflight__"
    event_records = [json.loads(line) for line in events.read_text().splitlines()]
    assert event_records[-1]["event"] == "run_end"
    assert event_records[-1]["final_outcome"] == "error"
    with sqlite3.connect(cxdb_path) as connection:
        assert connection.execute(
            "SELECT final FROM runs WHERE run_id = ?", (payload["run_id"],)
        ).fetchone() == ("error",)
