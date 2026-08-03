from __future__ import annotations

import pathlib
import sys
from types import SimpleNamespace

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))


def _pipeline(tmp_path: pathlib.Path) -> pathlib.Path:
    dot = tmp_path / "smoke.dot"
    dot.write_text(
        "digraph smoke {\n"
        '  graph [goal="shadow default smoke"]\n'
        "  start [shape=Mdiamond]\n"
        "  exit [shape=Msquare]\n"
        "  start -> exit\n"
        "}\n",
        encoding="utf-8",
    )
    return dot


def test_cli_disables_shadow_codex_review_by_default(tmp_path, monkeypatch, capsys):
    from runner import __main__ as cli

    seen = {}

    def _fake_run(graph, ctx, **kwargs):
        seen["state"] = dict(ctx.state)
        return [
            SimpleNamespace(
                node="exit",
                outcome="success",
                output_preview="exit",
                metadata={},
            )
        ]

    monkeypatch.setattr(cli, "run", _fake_run)

    rc = cli.main([
        "--pipeline",
        str(_pipeline(tmp_path)),
        "--goal",
        "x",
        "--backend",
        "echo",
        "--no-perf-log",
    ])

    assert rc == 0
    assert seen["state"]["_df_shadow_codex_review"] == "false"
    capsys.readouterr()


def test_cli_shadow_codex_review_can_be_explicitly_enabled(tmp_path, monkeypatch, capsys):
    from runner import __main__ as cli

    seen = {}

    def _fake_run(graph, ctx, **kwargs):
        seen["state"] = dict(ctx.state)
        return [
            SimpleNamespace(
                node="exit",
                outcome="success",
                output_preview="exit",
                metadata={},
            )
        ]

    monkeypatch.setattr(cli, "run", _fake_run)

    rc = cli.main([
        "--pipeline",
        str(_pipeline(tmp_path)),
        "--goal",
        "x",
        "--backend",
        "echo",
        "--state",
        "_df_shadow_codex_review=true",
        "--no-perf-log",
    ])

    assert rc == 0
    assert seen["state"]["_df_shadow_codex_review"] == "true"
    capsys.readouterr()
