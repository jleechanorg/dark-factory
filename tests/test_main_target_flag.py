from __future__ import annotations

import pathlib
import sys
import types

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT  # noqa: E402, F811

from runner.__main__ import main  # noqa: E402


def _fake_history_record(outcome: str = "success"):
    return types.SimpleNamespace(
        node="start", outcome=outcome, output_preview="ok", metadata={}
    )


def _stub_run(monkeypatch, captured: dict):
    def fake_run(graph, ctx, **kwargs):
        captured["ctx"] = ctx
        return [_fake_history_record()]

    monkeypatch.setattr("runner.__main__.run", fake_run)


def _reviewer_first_pipeline(tmp_path: pathlib.Path) -> pathlib.Path:
    """Finding 5 (round 3): `--target` now refuses any pipeline whose
    `start` node doesn't lead directly to a verdict-gated review node
    (reviewer-first entry-mode wiring). No shipped pipeline is
    reviewer-first yet, so target-mode CLI tests need their own minimal
    fixture graph rather than `pipelines/factory/hello.dot` (worker-first)."""
    prompt = tmp_path / "review.md"
    prompt.write_text("Review target: ${target}\n${intent}\nVerdict: PASS or Verdict: FAIL.\n")
    dot = tmp_path / "reviewer_first.dot"
    dot.write_text(
        f"""
digraph ReviewerFirst {{
    graph [goal="target-mode smoke fixture"]
    start [shape=Mdiamond, label="Start"]
    exit  [shape=Msquare,  label="Exit"]
    cold_reviewer [
        type="codergen", class="review", backend="codex",
        verdict_gate="true", prompt="@{prompt}", timeout=60
    ]
    start -> cold_reviewer
    cold_reviewer -> exit [condition="outcome=success"]
}}
"""
    )
    return dot


class TestTargetFlagResolution:
    def test_unresolvable_target_refuses_to_start(self, tmp_path, capsys):
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--target", "this is not resolvable to anything at all",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "--target" in err

    def test_defined_not_resolvable_scheme_refuses_to_start(self, tmp_path, capsys):
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--target", "bead://abc123",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "not resolvable in v1" in err

    def test_valid_file_target_sets_ctx_state_and_makes_goal_optional(
        self, tmp_path, monkeypatch, capsys
    ):
        target_file = tmp_path / "spec.md"
        target_file.write_text("spec body")
        captured: dict = {}
        _stub_run(monkeypatch, captured)
        pipeline = _reviewer_first_pipeline(tmp_path)

        rc = main([
            "--pipeline", str(pipeline),
            "--target", str(target_file),
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--no-perf-log",
            "--evidence-bundle", str(tmp_path / "evidence"),
        ])

        assert rc == 0
        ctx = captured["ctx"]
        assert ctx.state["target"].startswith("file://")
        assert ctx.state["_df_target_mode"] == "true"
        assert ctx.goal == ""

    def test_scheme_uri_target_delegates_to_strict_parse(
        self, tmp_path, monkeypatch
    ):
        target_file = tmp_path / "spec.md"
        target_file.write_text("body")
        import hashlib
        digest = hashlib.sha256(b"body").hexdigest()
        captured: dict = {}
        _stub_run(monkeypatch, captured)
        pipeline = _reviewer_first_pipeline(tmp_path)

        rc = main([
            "--pipeline", str(pipeline),
            "--target", f"file://{target_file}@sha256:{digest}",
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--no-perf-log",
            "--evidence-bundle", str(tmp_path / "evidence"),
        ])

        assert rc == 0
        assert captured["ctx"].state["target"] == f"file://{target_file.resolve()}@sha256:{digest}"

    def test_goal_still_required_without_target(self, tmp_path, capsys):
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "--goal is required" in err

    def test_target_and_goal_are_mutually_exclusive(self, tmp_path, capsys):
        """D5 (v3.1 delta): combining --target and --goal is an argparse
        error — a target-mode verification run has no free-text goal."""
        target_file = tmp_path / "spec.md"
        target_file.write_text("body")
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--target", str(target_file),
                "--goal", "do something",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "mutually exclusive" in err

    def test_target_refuses_worker_first_pipeline_instead_of_morphing_into_task_mode(
        self, tmp_path, capsys
    ):
        """Finding 5 (external review, round 3): a pipeline whose `start`
        leads to a worker (not a verdict-gated review node) has no
        reviewer-first entry-mode wiring — `--target` must refuse to run
        rather than silently run the worker on the local workdir and let
        the first post-worker mint discard the resolved target."""
        target_file = tmp_path / "spec.md"
        target_file.write_text("spec body")
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",  # worker-first
                "--target", str(target_file),
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "reviewer-first" in err

    def test_pre_seeded_state_intent_cannot_alter_the_rendered_envelope(self, tmp_path, capsys):
        """D2 fail-closed (external-review CRITICAL finding, round 3):
        `${intent}` must be sourced ONLY from the runner-recorded run-start
        intent envelope, never caller-supplied `--state`. `--state
        intent=...` is refused outright at the CLI boundary rather than
        silently accepted and later raced by (or masking) the real mint."""
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--goal", "do something",
                "--state", "intent=aGFja2VkIGludGVudA==",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "reserved key 'intent'" in err

    def test_target_intent_requires_target(self, tmp_path, capsys):
        """`/factory-review` (/fr) needs a way to carry the calling LLM's
        task description into a --target run's ${intent}; --target-intent
        is that dedicated flag and must refuse to run without --target."""
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--goal", "do something",
                "--target-intent", "reviewed change description",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert "--target-intent requires --target" in err

    def test_target_intent_sets_base64_intent_state(self, tmp_path, monkeypatch):
        """--target-intent free text lands in ctx.state['intent'] as the
        same Base64-encoded envelope shape _mint_post_worker_target uses
        for a worker's --goal (D2), so review_only.dot's cold_reviewer
        renders a real task record instead of the default placeholder."""
        import base64

        target_file = tmp_path / "spec.md"
        target_file.write_text("spec body")
        captured: dict = {}
        _stub_run(monkeypatch, captured)
        pipeline = _reviewer_first_pipeline(tmp_path)

        rc = main([
            "--pipeline", str(pipeline),
            "--target", str(target_file),
            "--target-intent", "added a caching layer; please review",
            "--backend", "echo",
            "--workdir", str(tmp_path),
            "--no-perf-log",
            "--evidence-bundle", str(tmp_path / "evidence"),
        ])

        assert rc == 0
        ctx = captured["ctx"]
        decoded = base64.b64decode(ctx.state["intent"]).decode("utf-8")
        assert decoded == "added a caching layer; please review"

    @pytest.mark.parametrize(
        "reserved_key",
        [
            "intent",
            "target",
            "_target_base_sha",
            "_target_pin_chain",
            "_target_mint_failed",
            "_df_mint_review_target",
            "_df_target_mode",
            "_pre_worker_head",
        ],
    )
    def test_state_refuses_every_reserved_key(self, reserved_key, tmp_path, capsys):
        """Round 5: extends the `intent` refusal to every key the
        review-target/intent integrity chain owns (D2/D3/D8a) — a caller
        must not be able to fabricate a matching pin chain, a fake mint
        result, or flip the mint/target-mode gates via `--state`."""
        with pytest.raises(SystemExit) as exc_info:
            main([
                "--pipeline", "pipelines/factory/hello.dot",
                "--goal", "do something",
                "--state", f"{reserved_key}=whatever",
                "--backend", "echo",
                "--workdir", str(tmp_path),
                "--no-perf-log",
            ])
        assert exc_info.value.code == 2
        err = capsys.readouterr().err
        assert f"reserved key {reserved_key!r}" in err
