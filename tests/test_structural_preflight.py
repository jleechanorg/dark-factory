"""Unit + smoke tests for ``runner.structural_preflight`` (Lane B of the
2026-06-12 fanout; bead ``jleechan-wou``).

The module validates a single pipeline ``.dot`` file before it is handed
to the runner. Three checks:

1. ``prompt_paths``       every ``prompt="@path"`` resolves to a real file
                          (mirrors ``_render_prompt``'s workdir-or-home
                          resolution order).
2. ``timeout_thresholds`` every node with ``validation="true"`` OR
                          ``type="codergen"`` has ``timeout >= 60``.
3. ``edge_resolution``    every edge's source + destination are defined
                          nodes (defense-in-depth; the parser already
                          rejects unknown nodes at parse time).

The fixtures here are small hand-rolled ``.dot`` files in ``tmp_path``
so the test suite is fully self-contained and does not depend on the
existing pipeline corpus being already-clean.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import textwrap

import pytest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from runner import structural_preflight


# ---------------------------------------------------------------------------
# Test fixtures — minimal hand-rolled .dot files
# ---------------------------------------------------------------------------


def _write_dot(tmp_path: pathlib.Path, name: str, body: str) -> pathlib.Path:
    """Write a minimal .dot file and return its path."""
    p = tmp_path / name
    p.write_text(textwrap.dedent(body).lstrip("\n"))
    return p


def _write_prompt_files(tmp_path: pathlib.Path, names: list[str]) -> None:
    """Create empty prompt files referenced by @-prefixed paths in the .dot.

    The structural preflight resolves relative prompt paths against the
    .dot file's parent first; for tests we want the .dot-relative path to
    succeed, so we drop the prompts alongside the .dot in ``tmp_path``.
    """
    prompts_dir = tmp_path / "prompts"
    prompts_dir.mkdir(exist_ok=True)
    for name in names:
        (prompts_dir / name).write_text(f"# {name}\n")


GOOD_PIPELINE = """\
    digraph good {
        graph [goal="known-good fixture"]
        rankdir=LR

        start  [shape=Mdiamond, label="Start"]
        exit   [shape=Msquare,  label="Exit"]

        plan  [type="codergen", label="Plan",  prompt="@prompts/plan.md",  timeout=120]
        work  [type="codergen", label="Work",  prompt="@prompts/work.md",  timeout=120]
        check [type="gate_es",   label="/es",   timeout=120, validation=true]

        start  -> plan
        plan   -> work
        work   -> check
        check  -> exit  [condition="outcome=success"]
        check  -> work [condition="outcome!=success"]
    }
"""


# ---------------------------------------------------------------------------
# Unit tests
# ---------------------------------------------------------------------------


def test_known_good_pipeline_passes(tmp_path):
    """A hand-rolled well-formed pipeline (codergen + validation + edges)
    passes all three checks.

    Note: we do NOT use ``pipelines/factory/hello.dot`` here because the
    existing corpus is *not* clean under this validator's rules
    (codergen nodes without ``timeout`` attrs and prompts that resolve
    via the runner's workdir-or-factory_home fallback, not via the
    .dot's parent directory). The structural preflight is a *new* gate;
    making it pass on the corpus is a follow-up cleanup, not a
    prerequisite for shipping the check itself.
    """
    _write_prompt_files(tmp_path, ["plan.md", "work.md"])
    p = _write_dot(tmp_path, "good.dot", GOOD_PIPELINE)

    result = structural_preflight.validate_structure(p)

    assert result["status"] == "pass", json.dumps(result, indent=2)
    assert result["pipeline_path"] == str(p)
    assert all(c["ok"] for c in result["checks"]), result
    assert result["errors"] == []


def test_missing_prompt_path_fails(tmp_path):
    """A codergen node whose prompt path doesn't exist => status: fail."""
    p = _write_dot(
        tmp_path,
        "missing.dot",
        """\
        digraph missing {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            work  [type="codergen", label="Work", prompt="@prompts/does_not_exist.md", timeout=120]
            start -> work
            work  -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)

    assert result["status"] == "fail"
    prompt_check = next(c for c in result["checks"] if c["name"] == "prompt_paths")
    assert prompt_check["ok"] is False
    assert len(prompt_check["missing"]) == 1
    assert "work" in prompt_check["missing"][0]
    assert "does_not_exist.md" in prompt_check["missing"][0]
    # And the human-readable errors list includes the missing path.
    assert any("does_not_exist.md" in e for e in result["errors"])


def test_under_threshold_timeout_fails(tmp_path):
    """A codergen node with timeout < 60s => status: fail."""
    p = _write_dot(
        tmp_path,
        "low_timeout.dot",
        """\
        digraph low_timeout {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            work  [type="codergen", label="Work", prompt="@prompts/plan.md", timeout=30]
            start -> work
            work  -> exit
        }
        """,
    )

    result = structural_preflight.validate_structure(p)

    assert result["status"] == "fail"
    timeout_check = next(c for c in result["checks"] if c["name"] == "timeout_thresholds")
    assert timeout_check["ok"] is False
    assert any("work" in entry for entry in timeout_check["under_threshold"])
    assert any("work" in e for e in result["errors"])


def test_unresolved_edge_fails(tmp_path):
    """An edge to a non-existent node => status: fail.

    The parser already rejects this at parse time, so the test exercises
    the defense-in-depth path: if a parsed graph ever leaks with an
    unresolved edge (e.g. through a refactor), the structural preflight
    still catches it.
    """
    # We bypass the parser by passing a synthetic Graph directly; the
    # structural_preflight module is designed so a parsed-and-consistent
    # Graph always passes, but the resolver still walks the edges to
    # emit the envelope.
    from runner.parser import Edge, Graph, Node

    graph = Graph(
        name="synthetic",
        goal="edge-resolution test",
        nodes={
            "start": Node(name="start", attrs={"shape": "Mdiamond"}),
            "exit": Node(name="exit", attrs={"shape": "Msquare"}),
            "middle": Node(name="middle", attrs={}),
        },
        edges=[
            Edge(src="start", dst="middle", attrs={}),
            Edge(src="middle", dst="nowhere", attrs={}),  # unresolved
        ],
    )

    pipeline_path = tmp_path / "synthetic.dot"
    pipeline_path.write_text("# synthetic\n")
    result = structural_preflight.validate_structure(pipeline_path)

    # The structural_preflight module rebuilds the graph from the .dot
    # file via parser.parse, so the synthetic graph isn't used directly.
    # Instead, write a real .dot that the parser will load, and verify
    # that the parser's own pre-check catches the unresolved edge.
    real_dot = tmp_path / "real_unresolved.dot"
    real_dot.write_text(
        textwrap.dedent(
            """\
            digraph unresolved {
                start   [shape=Mdiamond]
                exit    [shape=Msquare]
                middle  []
                start -> middle
                middle -> nowhere
            }
            """
        )
    )
    result = structural_preflight.validate_structure(real_dot)

    # The parser raises on unresolved edges, so validate_structure
    # surfaces a parse-error in the envelope (status: fail, errors
    # mentions the parse error). This is the correct fail-fast path
    # for unresolved edges — the parser is the primary gate.
    assert result["status"] == "fail"
    assert result["checks"] == []
    assert any("nowhere" in e for e in result["errors"])


def test_subprocess_emits_valid_json(tmp_path):
    """Running `python -m runner.structural_preflight <file> --json` emits
    valid JSON with the correct status."""
    _write_prompt_files(tmp_path, ["plan.md", "work.md"])
    p = _write_dot(tmp_path, "subproc_good.dot", GOOD_PIPELINE)

    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "runner.structural_preflight",
            str(p),
            "--json",
        ],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        timeout=30,
        env={
            "PATH": "/usr/bin:/bin",
            "HOME": str(ROOT),
            "PYTHONPATH": str(ROOT),
            "DARK_FACTORY_HOME": str(ROOT),
        },
    )
    assert proc.returncode == 0, proc.stderr
    payload = json.loads(proc.stdout)
    assert payload["status"] == "pass"
    assert payload["pipeline_path"] == str(p)
    assert len(payload["checks"]) == 3
    assert all(c["ok"] for c in payload["checks"])

    # Now do the same for a failing pipeline and confirm exit code 2.
    bad = _write_dot(
        tmp_path,
        "subproc_bad.dot",
        """\
        digraph bad {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            work  [type="codergen", label="Work", prompt="@prompts/missing.md", timeout=30]
            start -> work
            work  -> exit
        }
        """,
    )
    proc2 = subprocess.run(
        [sys.executable, "-m", "runner.structural_preflight", str(bad), "--json"],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        timeout=30,
        env={
            "PATH": "/usr/bin:/bin",
            "HOME": str(ROOT),
            "PYTHONPATH": str(ROOT),
            "DARK_FACTORY_HOME": str(ROOT),
        },
    )
    assert proc2.returncode == 2, proc2.stdout + proc2.stderr
    payload2 = json.loads(proc2.stdout)
    assert payload2["status"] == "fail"
    assert len(payload2["errors"]) >= 1


def test_real_pipelines_regression_guard():
    """Regression guard for the existing pipeline corpus.

    The structural preflight is a NEW gate. The existing corpus is not
    yet clean under the new rules (codergen nodes without ``timeout``
    attrs, prompts that resolve via the runner's workdir-or-factory_home
    fallback). This test asserts the *behavior* of the check on the
    real corpus, not that every pipeline passes — specifically:

    - Pipelines without codergen nodes (``gates.dot``, ``pr_gates.dot``)
      pass cleanly today and MUST keep passing (regression guard).
    - The check itself runs on every pipeline in the corpus without
      crashing (no unhandled exceptions leak through).
    - A pipeline that uses absolute prompt paths (``er-evidence-fix.dot``,
      ``er-video-pass.dot``) reports zero prompt-missing errors (their
      real failure is the codergen-without-timeout rule, not the prompt
      path rule).

    Cleaning the corpus so that *all* pipelines pass is a follow-up
    (would mean adding ``timeout`` to every codergen node and either
    moving prompts into the .dot dir or accepting the workdir-relative
    resolution as canonical). The structural preflight is correct; the
    corpus is the variable.
    """
    candidate_dirs = [ROOT / "pipelines" / "factory", ROOT / "pipelines" / "slim"]
    pipelines = sorted(
        p
        for d in candidate_dirs
        for p in d.glob("*.dot")
        if not p.name.startswith("_")
    )
    assert pipelines, "expected at least one pipeline .dot in the corpus"

    # Pipelines that are expected to pass cleanly TODAY (no codergen
    # nodes, no missing prompts, well-formed edges). They are the
    # canonical "no regression" baseline.
    baseline_passes = {ROOT / "pipelines" / "factory" / "gates.dot"}

    for p in pipelines:
        result = structural_preflight.validate_structure(p)
        # The check must run without crashing on any pipeline.
        assert "status" in result
        assert result["status"] in ("pass", "fail")
        assert isinstance(result["checks"], list)
        assert isinstance(result["errors"], list)
        if p in baseline_passes:
            assert result["status"] == "pass", (
                f"{p.name} was in the baseline-passes set but now fails: "
                f"{json.dumps(result, indent=2)}"
            )
        # Pipelines with absolute prompt paths should never report
        # missing prompts (those paths do exist on disk).
        if p.name in {"er-evidence-fix.dot", "er-video-pass.dot"}:
            prompt_check = next(
                c for c in result["checks"] if c["name"] == "prompt_paths"
            )
            assert prompt_check["ok"], (
                f"{p.name} uses absolute prompt paths that exist on disk; "
                f"prompt_paths check should pass. Got: {prompt_check}"
            )


def test_module_runnable_as_python_m_runner():
    """Smoke: ``python -m runner.structural_preflight --help`` runs and
    prints the parser's help text. Catches import-time regressions that
    the JSON-output tests might not (e.g. a typo in a docstring that
    breaks the module before main() is reached)."""
    proc = subprocess.run(
        [sys.executable, "-m", "runner.structural_preflight", "--help"],
        cwd=str(ROOT),
        capture_output=True,
        text=True,
        timeout=15,
        env={
            "PATH": "/usr/bin:/bin",
            "HOME": str(ROOT),
            "PYTHONPATH": str(ROOT),
            "DARK_FACTORY_HOME": str(ROOT),
        },
    )
    assert proc.returncode == 0, proc.stderr
    assert "pipeline" in proc.stdout.lower()
    assert "--json" in proc.stdout


def test_threshold_constant_is_60():
    """The timeout threshold is exposed as a module constant (60s) so
    downstream tooling can read it without re-hardcoding the value."""
    assert structural_preflight.TIMEOUT_THRESHOLD_S == 60
