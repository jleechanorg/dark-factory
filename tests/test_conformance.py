from __future__ import annotations

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import run_conformance  # noqa: E402


def test_conformance_parse_outputs_stable_ast():
    proc = run_conformance("parse", "pipelines/factory/hello.dot", timeout=120)

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["name"] == "hello"
    assert payload["nodes"]
    assert payload["edges"]
    assert all("id" in node for node in payload["nodes"])
    assert all("from" in edge and "to" in edge for edge in payload["edges"])


def test_conformance_validate_outputs_machine_readable_json():
    proc = run_conformance("validate", "pipelines/factory/hello.dot")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert "diagnostics" in payload
    assert isinstance(payload["diagnostics"], list)


def test_conformance_validate_rejects_bad_dot(tmp_path):
    bad_dot = tmp_path / "bad.dot"
    bad_dot.write_text("digraph bad { start; start -> missing; }")

    proc = run_conformance("validate", str(bad_dot))

    assert proc.returncode == 1
    payload = json.loads(proc.stdout)
    assert "diagnostics" in payload
    assert isinstance(payload["diagnostics"], list)
    assert len(payload["diagnostics"]) > 0


def test_conformance_run_uses_echo_backend_and_zero_cost():
    proc = run_conformance("run", "pipelines/factory/hello.dot", "--feature", "hello")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "success"
    assert payload["outcome"] == "success"
    assert payload["cost"] == {"api_calls": 0, "tokens": 0}


def test_conformance_list_handlers():
    proc = run_conformance("list-handlers")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert isinstance(payload, list)
    text = json.dumps(payload)
    assert "codergen" in text
    assert "holdout_eval" in text


def test_conformance_score_is_deterministic_mock_surface():
    proc = run_conformance("score")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["subcommand"] == "score"
    assert payload["status"] == "pass"
    assert payload["scope"] == "local_mock"
    assert payload["tiers"]["sealed_benchmark"] == "not_run"
    assert payload["cost"] == {"api_calls": 0, "tokens": 0}


def test_conformance_run_supports_mock_url():
    proc = run_conformance("run", "pipelines/factory/hello.dot", "--mock-url", "http://127.0.0.1:54321")
    assert proc.returncode == 0, proc.stdout + proc.stderr


def test_conformance_validate_walker_skips_underscore_dot_libraries():
    """`conformance validate` (no args) walks the pipelines + benchmarks trees
    and rejects any file without `start`/`exit`. Library fragments
    (`_*.dot`, e.g. `pipelines/_base.dot`) are deliberately free of those
    nodes — they are included by lanes via `include="@..."` and parsed with
    `require_start_exit=False`. The walker must filter them out so the
    strict parser invariant is preserved while `conformance score` (CI Gate
    2) stays green. Regression test for bead jleechan-u8e."""
    # 1. The walker must succeed on the full tree. Level-5 soft-tier
    # `warning` diagnostics (healer/spec_validation/holdout_eval opt-outs)
    # are expected on factory/*.dot files per the G3 closure design —
    # filter them out and assert the walker emits no *errors*.
    proc = run_conformance("validate")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    error_diags = [d for d in payload["diagnostics"] if d.get("severity") == "error"]
    assert error_diags == [], error_diags

    # 2. The parser invariant is still strict: explicit validate of a
    # library fragment must fail (so authors cannot accidentally run a
    # `_base.dot`-style file as a top-level pipeline).
    proc_explicit = run_conformance("validate", "pipelines/_base.dot")
    assert proc_explicit.returncode == 1, proc_explicit.stdout + proc_explicit.stderr
    payload_explicit = json.loads(proc_explicit.stdout)
    assert any(
        diag.get("severity") == "error" and "start" in diag.get("message", "").lower()
        for diag in payload_explicit["diagnostics"]
    ), payload_explicit


# ---------------------------------------------------------------------------
# Level-5 rule set — see project_2026-06-22_g3_closure_dynamic_node_design.md
# ---------------------------------------------------------------------------


def test_level5_valid_passes():
    proc = run_conformance("validate", "tests/fixtures/level5_valid.dot")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["diagnostics"] == [], payload


def test_level5_factory_graphs_accept_parallel_reviewer_hard_tier():
    """Typed parallel reviewer nodes satisfy the cross-vendor hard tier."""
    for path in (
        "pipelines/factory/gates.dot",
        "pipelines/factory/pr_gates.dot",
        "pipelines/factory/level5_feature.dot",
    ):
        proc = run_conformance("validate", path)
        assert proc.returncode == 0, proc.stdout + proc.stderr
        payload = json.loads(proc.stdout)
        error_diags = [d for d in payload["diagnostics"] if d.get("severity") == "error"]
        assert error_diags == [], (path, payload)


def test_level5_missing_gate_fails():
    proc = run_conformance("validate", "tests/fixtures/level5_missing_gate.dot")
    assert proc.returncode == 1, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    rules = {d.get("rule") for d in payload["diagnostics"]}
    assert "missing_hard_tier_gate" in rules, payload
    assert any(
        d.get("severity") == "error" and "gate_er" in d.get("message", "")
        for d in payload["diagnostics"]
    ), payload


def test_level5_with_skip_passes():
    proc = run_conformance("validate", "tests/fixtures/level5_with_skip.dot")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["diagnostics"] == [], payload


def test_level5_multi_class_role_attributes_pass():
    """`class` is a space- or comma-separated token list, not a scalar.

    Real graphs combine the role token with routing/styling classes
    (`class="codergen explore"` or `class="codergen,implement"`). The
    coding-role check must tokenize before comparing, the same way
    `runner.parser._selector_matches` does — otherwise valid graphs
    false-fail with `missing_coding_role`. Regression test for the Codex
    cold-review finding on PR #130.
    """
    proc = run_conformance("validate", "tests/fixtures/level5_multi_class.dot")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    error_diags = [d for d in payload["diagnostics"] if d.get("severity") == "error"]
    assert error_diags == [], payload


def test_slim_pipelines_exempt_from_level5():
    """Slim pipelines must remain free of Level-5 hard-tier enforcement."""
    proc = run_conformance("validate", "pipelines/slim/minimal_feature.dot")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert all(
        d.get("rule") != "missing_hard_tier_gate"
        for d in payload["diagnostics"]
    ), payload


def test_hello_dot_exempt_from_level5():
    """`hello.dot` is the smoke lane and must stay exempt from Level-5."""
    proc = run_conformance("validate", "pipelines/factory/hello.dot")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    assert all(
        d.get("rule") != "missing_hard_tier_gate"
        for d in payload["diagnostics"]
    ), payload


def test_graph_level5_true_attribute_enables_rule(tmp_path):
    """A `.dot` with `graph [level5="true"]` triggers the rule check
    regardless of location — used by tests + ad-hoc author validation."""
    level5_dot = tmp_path / "adhoc_level5.dot"
    level5_dot.write_text(
        'digraph adhoc {\n'
        '    graph [level5="true", backend="claude"]\n'
        '    rankdir=LR\n'
        '    start [shape=Mdiamond, label="Start"]\n'
        '    exit  [shape=Msquare,  label="Exit"]\n'
        '    start -> exit\n'
        '}\n'
    )
    proc = run_conformance("validate", str(level5_dot))
    assert proc.returncode == 1, proc.stdout + proc.stderr
    payload = json.loads(proc.stdout)
    rules = [d.get("rule") for d in payload["diagnostics"]]
    assert "missing_hard_tier_gate" in rules, payload
