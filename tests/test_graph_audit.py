"""Tests for ``runner/graph_audit`` — static structural G1+G2 audit.

Three fixture .dot files under ``tests/fixtures/graph_audit/`` cover the
contract surface:

* ``clean.dot``        — both G1 and G2 clean.
* ``g1_violator.dot``  — codergen reaches exit without any reviewer.
* ``g2_violator.dot``  — reviewer routes ``outcome!=success`` to exit.

The handler-resolution unit tests use ``pytest.mark.parametrize`` so
adding a new (type, is_reviewer, is_codergen) case is a one-line table
edit instead of a new fixture file (anti-creation note).
"""

from __future__ import annotations

import pathlib
import sys
import textwrap

import pytest

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))

from runner import graph_audit
from runner.parser import Node
from tests.conftest import make_node


FIXTURES = ROOT / "tests" / "fixtures" / "graph_audit"


# ---------------------------------------------------------------------------
# Handler-resolution mirror — exact contract test from CLAUDE.md
# § "Handlers": explicit ``type=`` wins over start/exit, start/exit
# wins over shape, shape wins over default. These tests pin that order
# so the audit's classification cannot drift from the engine's
# dispatch.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "name, attrs, want_label",
    [
        # 1. Explicit type wins (TYPE_REGISTRY lookup).
        ("coder", {"type": "tool"}, "tool"),
        ("coder", {"type": "gate_es"}, "gate_es"),
        ("coder", {"type": "gate_er"}, "gate_er"),
        ("coder", {"type": "gate_code_standards"}, "gate_code_standards"),
        ("coder", {"type": "holdout_eval"}, "holdout_eval"),
        ("coder", {"type": "conditional"}, "conditional"),
        ("coder", {"type": "parallel"}, "parallel"),
        ("coder", {"type": "join"}, "join"),
        # 2. Name-based start/exit short-circuit (wins over shape).
        ("start", {"shape": "ellipse"}, "start"),
        ("exit", {"shape": "ellipse"}, "exit"),
        # 3. Shape-only registration wins over default.
        ("cond", {"shape": "hexagon"}, "hexagon"),
        ("fan", {"shape": "component"}, "component"),
        ("join", {"shape": "tripleoctagon"}, "tripleoctagon"),
        # 4. Default fallback.
        ("coder", {}, "codergen"),
        ("coder", {"prompt": "@prompts/x.md"}, "codergen"),
    ],
)
def test_resolved_type_label_matches_engine(name, attrs, want_label):
    """Pin the audit's handler-resolution order to the engine's:

    explicit ``type=`` → name (start/exit) → shape → default ``codergen``.
    The label is the registry KEY (string), not the handler function
    name — the audit only compares it against ``_REVIEWER_TYPE_NAMES``
    and the literal ``"codergen"``.
    """
    node = make_node(name, **attrs)
    assert graph_audit._resolved_type_label(node) == want_label


@pytest.mark.parametrize(
    "attrs, is_code, is_review",
    [
        # type="tool" — engine dispatches to _tool, NOT _codergen.
        ({"type": "tool"}, False, False),
        # type="gate_es" — reviewer.
        ({"type": "gate_es"}, False, True),
        # type="gate_er" — reviewer.
        ({"type": "gate_er"}, False, True),
        # type="gate_skeptic" — reviewer.
        ({"type": "gate_skeptic"}, False, True),
        # type="gate_code_standards" — reviewer.
        ({"type": "gate_code_standards"}, False, True),
        # type="holdout_eval" — reviewer (the only behavioral gate).
        ({"type": "holdout_eval"}, False, True),
        # type="gate_red" — gate, but NOT in _REVIEWER_TYPE_NAMES.
        ({"type": "gate_red"}, False, False),
        # type="gate_green" — same, NOT a reviewer per the G1/G2
        # taxonomy (red/green are test-pass/fail gates, not whole-diff
        # review gates).
        ({"type": "gate_green"}, False, False),
        # type="parallel" with shape=component — topology-only.
        ({"type": "parallel", "shape": "component"}, False, False),
        # shape=point — routing anchor, never code-producing.
        ({"shape": "point"}, False, False),
        # No type, no shape — default codergen.
        ({}, True, False),
        # class="review" on a codergen — STILL not a reviewer (the
        # engine never resolves class to a handler; only type/shape
        # participate in dispatch).
        ({"type": "codergen", "class": "review"}, True, False),
    ],
)
def test_classification_table(attrs, is_code, is_review):
    node = make_node("n", **attrs)
    assert graph_audit._is_code_producing(node) is is_code
    assert graph_audit._is_reviewer(node) is is_review


@pytest.mark.parametrize(
    "attrs, expected",
    [
        ({"type": "codergen", "backend": "claude"}, True),
        ({"type": "codergen", "backend": "claude-sonnet"}, True),
        ({"type": "codergen", "model": "claude"}, True),
        ({"type": "codergen", "model": "claude-sonnet"}, True),
        ({"type": "codergen", "backend": "claudem"}, False),
        ({"type": "codergen", "backend": "minimax"}, False),
        ({"type": "codergen", "model_name": "claude-sonnet-4-6"}, False),
        ({"type": "gate_er", "backend_priority": "codex,claude"}, True),
        ({"type": "gate_er", "backend_priority": "codex,claude-sonnet"}, True),
        ({"type": "gate_er", "backend_priority": "codex,claude", "explicit_claude_lane": "true"}, True),
        ({"type": "web_advice"}, True),
    ],
)
def test_direct_claude_route_detection_is_exact(attrs, expected):
    node = make_node("n", **attrs)
    assert graph_audit._is_direct_claude_route(node) is expected


def test_direct_claude_route_requires_both_scope_markers(tmp_path):
    p = _write_dot(tmp_path, "claude_scope.dot", """
        digraph claude_scope {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            coder [type="codergen", backend="claude", explicit_claude_lane="true", requires_claude_config="false"]
            reviewer [type="gate_er"]
            start -> coder -> reviewer -> exit
        }
    """)
    violations = graph_audit.audit_graph(p)
    g5 = [v for v in violations if v.kind == "G5"]
    assert len(g5) == 1
    assert g5[0].location == "coder"


def test_direct_claude_route_with_both_scope_markers_passes(tmp_path):
    p = _write_dot(tmp_path, "claude_scope_ok.dot", """
        digraph claude_scope_ok {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            coder [type="codergen", backend="claude", explicit_claude_lane="true", requires_claude_config="true"]
            reviewer [type="gate_er"]
            start -> coder -> reviewer -> exit
        }
    """)
    assert not [v for v in graph_audit.audit_graph(p) if v.kind == "G5"]


def test_non_codergen_backend_claude_requires_scope_markers(tmp_path):
    """A non-codergen node can still select a direct Claude lane.

    The route audit must inspect the explicit backend attribute regardless of
    the resolved handler type; otherwise a reviewer/tool node can bypass the
    same project-scoped Claude account requirement enforced for codergen nodes.
    """
    p = _write_dot(tmp_path, "non_codergen_claude.dot", """
        digraph non_codergen_claude {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            coder [type="codergen", backend="codex"]
            reviewer [type="gate_er", backend="claude"]
            start -> coder -> reviewer -> exit
        }
    """)
    violations = graph_audit.audit_graph(p)
    g5 = [v for v in violations if v.kind == "G5"]
    assert len(g5) == 1, f"expected unscoped non-codergen Claude route, got {violations}"
    assert g5[0].location == "reviewer"


def test_audit_amazon_clone_pipelines_is_clean():
    violations = graph_audit.audit_graphs(ROOT / "benchmarks" / "amazon-clone" / "pipelines")
    # The benchmark bundle intentionally contains legacy graphs with unrelated
    # G1/G4 findings; this contract only asserts every direct Claude route is
    # explicitly scoped.
    assert not [v for v in violations if v.kind == "G5"], violations


def test_audit_repository_discovers_benchmark_claude_routes(tmp_path):
    """Repository-level audits include benchmark routes for G5 coverage.

    The production ``pipelines/`` audit root alone misses this nested graph.
    Existing benchmark G1/G4 design findings remain out of this route-scoped
    repository check, but an unscoped direct Claude route must be surfaced.
    """
    import shutil

    (tmp_path / "pipelines").mkdir()
    shutil.copy2(FIXTURES / "clean.dot", tmp_path / "pipelines" / "clean.dot")
    benchmark_pipelines = tmp_path / "benchmarks" / "example" / "pipelines"
    benchmark_pipelines.mkdir(parents=True)
    (benchmark_pipelines / "benchmark.dot").write_text(textwrap.dedent("""
        digraph benchmark {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            coder [type="codergen", backend="codex"]
            reviewer [type="gate_er", backend="claude"]
            start -> coder -> reviewer -> exit
        }
    """), encoding="utf-8")

    violations = graph_audit.audit_repository(tmp_path)
    benchmark_violations = [
        v for v in violations
        if pathlib.PurePosixPath(v.pipeline).parts[-4:-1]
        == ("benchmarks", "example", "pipelines")
    ]
    assert benchmark_violations, (
        "repository audit must discover benchmark pipeline graphs; "
        f"got {violations}"
    )
    assert [v.kind for v in benchmark_violations] == ["G5"]


def test_audit_repository_does_not_block_on_benchmark_g1_g4(tmp_path):
    """Legacy benchmark topology is not silently treated as production G1/G4."""
    import shutil

    (tmp_path / "pipelines").mkdir()
    shutil.copy2(FIXTURES / "clean.dot", tmp_path / "pipelines" / "clean.dot")
    benchmark_pipelines = tmp_path / "benchmarks" / "example" / "pipelines"
    benchmark_pipelines.mkdir(parents=True)
    shutil.copy2(FIXTURES / "g1_violator.dot", benchmark_pipelines / "legacy.dot")

    violations = graph_audit.audit_repository(tmp_path)
    assert not [v for v in violations if v.pipeline.endswith("legacy.dot")], violations


def test_audit_repository_discovers_hidden_dark_factory_slices(tmp_path):
    """Authored slice graphs under ``.dark-factory/`` are route-audited.

    These files are tracked factory inputs, unlike generated ``evidence/``
    captures and test fixtures.  Keep the discovery contract at the repository
    boundary so a future slice cannot reintroduce an unscoped Claude route.
    """
    hidden = tmp_path / ".dark-factory"
    hidden.mkdir()
    (hidden / "future-slice.dot").write_text(textwrap.dedent("""
        digraph future_slice {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            coder [type="codergen", backend="codex"]
            reviewer [type="gate_er", backend_priority="minimax,claude-sonnet"]
            start -> coder -> reviewer -> exit
        }
    """), encoding="utf-8")

    violations = graph_audit.audit_repository(tmp_path)
    g5 = [v for v in violations if v.kind == "G5"]
    assert len(g5) == 1, f"expected hidden slice G5 violation, got {violations}"
    assert g5[0].pipeline.endswith(".dark-factory/future-slice.dot")
    assert g5[0].location == "reviewer"


def test_tracked_dark_factory_slices_have_no_unscoped_claude_routes():
    """The two checked-in slices stay on the canonical non-personal queue."""
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    violations = graph_audit.audit_repository(repo_root)
    hidden_g5 = [
        v for v in violations
        if v.kind == "G5" and v.pipeline.startswith(".dark-factory/")
    ]
    assert hidden_g5 == [], hidden_g5


# ---------------------------------------------------------------------------
# Condition normalisation — pin that "outcome != success" is the only
# pattern G2 matches. A bare "outcome=success" edge is success routing
# (not a G2 hit). A complex condition like "outcome!=success AND x"
# is NOT flagged (engine would still treat it as a failure branch but
# the audit conservatively avoids false positives).
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "cond, expected",
    [
        ("outcome!=success", True),
        ("outcome != success", True),
        ('outcome!="success"', True),     # quoted RHS — STRING token
        ("outcome!='success'", True),     # single-quoted RHS
        ("outcome=success", False),
        ("outcome == success", False),
        (None, False),
        ("", False),
        ("outcome!=success AND x=1", False),  # complex — skip
        ("outcome!=warn", False),              # wrong RHS
        ("status!=success", False),            # wrong LHS
        ("malformed (((", False),              # bad token stream
    ],
)
def test_outcome_failure_condition(cond, expected):
    assert graph_audit._is_outcome_failure_condition(cond) is expected


# ---------------------------------------------------------------------------
# audit_graph integration — the three fixture .dot files.
# ---------------------------------------------------------------------------


def test_clean_graph_has_no_violations():
    violations = graph_audit.audit_graph(FIXTURES / "clean.dot")
    assert violations == [], f"expected no violations, got {violations}"


def test_failure_terminal_exit_is_not_an_unreviewed_success_path(tmp_path):
    """A fail-closed planner branch may terminate at exit without G1 noise."""
    p = _write_dot(tmp_path, "planner_failure_exit.dot", """
        digraph planner_failure_exit {
            start [shape=Mdiamond]
            exit [shape=Msquare]
            plan [type="codergen", backend="minimax"]
            reviewer [type="gate_er"]
            start -> plan
            plan -> reviewer [condition="outcome=success"]
            plan -> exit [condition="outcome!=success"]
            reviewer -> exit [condition="outcome=success"]
            reviewer -> plan [condition="outcome!=success"]
        }
    """)
    violations = graph_audit.audit_graph(p)
    assert not [v for v in violations if v.kind == "G1"], violations


def test_g1_violator_has_g1_violation_only():
    violations = graph_audit.audit_graph(FIXTURES / "g1_violator.dot")
    kinds = [v.kind for v in violations]
    assert "G1" in kinds, f"expected G1 violation, got {kinds}"
    assert "G2" not in kinds, f"unexpected G2 violation in {kinds}"
    g1 = next(v for v in violations if v.kind == "G1")
    assert "coder" in g1.location
    assert g1.message  # non-empty


def test_g2_violator_has_g2_violation_only():
    violations = graph_audit.audit_graph(FIXTURES / "g2_violator.dot")
    kinds = [v.kind for v in violations]
    assert "G2" in kinds, f"expected G2 violation, got {kinds}"
    assert "G1" not in kinds, f"unexpected G1 violation in {kinds}"
    g2 = next(v for v in violations if v.kind == "G2")
    assert "reviewer" in g2.location
    assert "exit" in g2.location


def test_advisory_allowlist_exempts_pipeline(tmp_path):
    """A .dot named in ADVISORY_ALLOWLIST must produce no violations
    even if structurally non-compliant."""
    # Build a G1-style violator but place it under a name in the
    # allowlist so we can verify the exemption.
    allowlisted = FIXTURES / "g1_violator.dot"
    # Mutate the module-level set locally for this test only.
    saved = set(graph_audit.ADVISORY_ALLOWLIST)
    try:
        rel = allowlisted.resolve().relative_to(pathlib.Path.cwd()).as_posix()
        graph_audit.ADVISORY_ALLOWLIST.add(rel)
        violations = graph_audit.audit_graph(allowlisted)
        assert violations == [], (
            f"allowlisted path should produce no violations, got {violations}"
        )
    finally:
        graph_audit.ADVISORY_ALLOWLIST.clear()
        graph_audit.ADVISORY_ALLOWLIST.update(saved)


def test_parse_failure_surfaces_as_violation(tmp_path):
    """Malformed .dot is reported as a parse-error violation, not
    silently swallowed."""
    bad = tmp_path / "bad.dot"
    bad.write_text("not a digraph at all (((", encoding="utf-8")
    violations = graph_audit.audit_graph(bad)
    assert any("parse" in v.message.lower() for v in violations), (
        f"expected parse-error message, got {violations}"
    )


# ---------------------------------------------------------------------------
# audit_graphs over a temp dir containing all three fixtures.
# ---------------------------------------------------------------------------


def test_audit_graphs_aggregates_across_fixtures(tmp_path):
    """Copy the 3 fixtures into a temp dir; expect exactly 2 violations:
    one G1 from g1_violator.dot and one G2 from g2_violator.dot."""
    import shutil

    target = tmp_path / "pipelines"
    target.mkdir()
    for name in ("clean.dot", "g1_violator.dot", "g2_violator.dot"):
        shutil.copy2(FIXTURES / name, target / name)

    violations = graph_audit.audit_graphs(target)
    kinds = sorted(v.kind for v in violations)
    # One G1 from g1_violator.dot (path start -> coder -> exit has
    # no reviewer); one G2 from g2_violator.dot (reviewer -> exit on
    # outcome!=success). clean.dot contributes nothing.
    assert kinds.count("G1") == 1, f"expected 1 G1, got {kinds}"
    assert kinds.count("G2") == 1, f"expected 1 G2, got {kinds}"


def test_audit_graphs_recurse_subdirs(tmp_path):
    """audit_graphs must recurse into subdirs (mirrors how the CLI
    sees pipelines/factory/, pipelines/slim/, and pipelines/_base.dot)."""
    import shutil

    root = tmp_path / "pipelines"
    root.mkdir()
    sub = root / "slim"
    sub.mkdir()
    shutil.copy2(FIXTURES / "g1_violator.dot", root / "g1_violator.dot")
    shutil.copy2(FIXTURES / "g2_violator.dot", sub / "g2_violator.dot")

    violations = graph_audit.audit_graphs(root)
    kinds = sorted(v.kind for v in violations)
    assert kinds.count("G1") == 1
    assert kinds.count("G2") == 1
    # Confirm the G2 message locates the subdir path.
    g2 = next(v for v in violations if v.kind == "G2")
    assert "slim" in g2.pipeline or "slim" in g2.location, (
        f"expected subdir path in G2 report, got {g2}"
    )


def test_audit_graphs_missing_dir_raises(tmp_path):
    with pytest.raises(FileNotFoundError):
        graph_audit.audit_graphs(tmp_path / "does_not_exist")


def test_audit_pipelines_dir_is_clean():
    """Regression: audit() on the REAL pipelines/ dir must return 0 violations.

    Lanes 1+2 of the factory-evolve work fixed all 6 red graphs. This test
    pins that state — if a future PR adds a new violator, this test fails
    and forces the fix (or an explicit allowlist update + bead).
    """
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    violations = graph_audit.audit_graphs(repo_root / "pipelines")
    assert violations == [], (
        f"Expected 0 violations in real pipelines/; got {len(violations)}:\n"
        + "\n".join(f"  {v.kind} {v.pipeline}:{v.location} {v.message}" for v in violations)
    )


# ---------------------------------------------------------------------------
# G3 / R1 — registered-handler enforcement (bead jleechan-0qy.5)
# ---------------------------------------------------------------------------


def _write_dot(tmp_path: pathlib.Path, name: str, body: str) -> pathlib.Path:
    p = tmp_path / name
    p.write_text(textwrap.dedent(body), encoding="utf-8")
    return p


def test_g3_registered_types_pass(tmp_path):
    """Every TYPE_REGISTRY key + the built-in fallbacks must NOT be
    flagged as unregistered. Confirms the G3/R1 check does not over-flag."""
    p = _write_dot(tmp_path, "clean.dot", textwrap.dedent("""\
        digraph clean {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            reviewer_es [type="gate_es", timeout=120]
            reviewer_er [type="gate_er", timeout=120]
            reviewer_skeptic [type="gate_skeptic", timeout=120]
            reviewer_holdout [type="holdout_eval", feature="hello", timeout=120]
            reviewer_parallel [type="parallel_reviewer", timeout=120]
            start -> reviewer_es -> reviewer_er -> reviewer_skeptic -> reviewer_holdout -> reviewer_parallel -> exit
        }
    """))
    violations = graph_audit.audit_graph(p)
    assert not any(v.kind in ("G3", "R1") for v in violations), (
        f"unexpected G3/R1 violations: "
        f"{[(v.kind, v.location, v.message) for v in violations if v.kind in ('G3', 'R1')]}"
    )


def test_g4_holdout_without_feature_fails(tmp_path):
    p = _write_dot(tmp_path, "missing_holdout_feature.dot", """\
        digraph missing_holdout_feature {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            holdout [type="holdout_eval", timeout=120]
            start -> holdout -> exit
        }
    """)

    violations = graph_audit.audit_graph(p)
    g4 = [v for v in violations if v.kind == "G4"]

    assert len(g4) == 1, f"expected 1 G4 violation, got {g4}"
    assert g4[0].location == "holdout"
    assert "feature" in g4[0].message


def test_g4_holdout_state_feature_passes(tmp_path):
    p = _write_dot(tmp_path, "state_feature_holdout.dot", """\
        digraph state_feature_holdout {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            holdout [type="holdout_eval", feature="${state.feature}", timeout=120]
            start -> holdout -> exit
        }
    """)

    violations = graph_audit.audit_graph(p)

    assert not any(v.kind == "G4" for v in violations), violations


def test_g3_gate_skeptic_unregistered_fails(tmp_path, monkeypatch):
    """The P0 bug class: type='gate_skeptic' without TYPE_REGISTRY entry
    must fail loudly. Without this guard, the runtime silently runs
    gate_skeptic as _codergen — a silent regression.

    We temporarily remove 'gate_skeptic' from TYPE_REGISTRY to simulate
    the pre-fix state and confirm the G3 check fires."""
    import runner.handlers as handlers_mod
    monkeypatch.delitem(handlers_mod.TYPE_REGISTRY, "gate_skeptic", raising=False)
    import importlib
    importlib.reload(graph_audit)
    try:
        fixture_path = (
            pathlib.Path(__file__).parent / "fixtures" / "graph_audit" / "g3_violator.dot"
        )
        violations = graph_audit.audit_graph(fixture_path)
        g3 = [v for v in violations if v.kind == "G3"]
        assert len(g3) == 1, f"expected 1 G3 violation, got {len(g3)}: {g3}"
        assert g3[0].location == "gate_skeptic"
        assert "TYPE_REGISTRY" in g3[0].message
    finally:
        importlib.reload(graph_audit)


def test_g3_library_fragment_is_exempt(tmp_path):
    """Files whose raw text contains `include="@..."` are include-only
    fragments (e.g. pipelines/_base.dot). They are NOT runnable, so the
    G3 contract does not apply — same exemption G1/G2 already get."""
    p = tmp_path / "_base.dot"
    p.write_text('include="@prompts/fragment.md"\n', encoding="utf-8")
    # Library-skip happens inside audit_graph() (see runner/graph_audit.py
    # parse-error branch — the `include="@"` raw-text check). Use it
    # directly because audit_graphs() expects a directory Path.
    violations = graph_audit.audit_graph(p)
    g3_or_r1 = [v for v in violations if v.kind in ("G3", "R1")]
    assert g3_or_r1 == [], (
        f"library fragment should be exempt from G3/R1; got: {g3_or_r1}"
    )


def test_gate_skeptic_counts_as_reviewer_for_g1(tmp_path):
    """Regression: gate_skeptic must satisfy reviewer coverage.

    The handler was registered for runtime dispatch, but graph_audit also
    needs to classify it as a reviewer so G1 does not flag valid Level-5
    graphs as missing review coverage.
    """
    p = _write_dot(tmp_path, "skeptic_reviewer.dot", """\
        digraph skeptic_reviewer {
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            coder [type="codergen", prompt="@prompts/slim/implement.md", timeout=120]
            gate_skeptic [type="gate_skeptic", timeout=120]
            start -> coder -> gate_skeptic -> exit
        }
    """)
    violations = graph_audit.audit_graph(p)
    assert not any(v.kind == "G1" for v in violations), (
        f"gate_skeptic should satisfy G1 reviewer coverage, got: {violations}"
    )


@pytest.mark.parametrize("bad_type", [
    "dynamic",
    "fake_typo_gate",
    "gate_erx",
    "parallel_reviwer",
    "hypothetical_future_gate",
])
def test_g3_unregistered_type_is_flagged(tmp_path, bad_type):
    """Any type='X' where X is not in TYPE_REGISTRY and not shape-resolvable
    must be flagged. Covers typos and hypothetical future types."""
    p = _write_dot(tmp_path, f"{bad_type}.dot", textwrap.dedent(f"""\
        digraph {bad_type} {{
            start [shape=Mdiamond]
            exit  [shape=Msquare]
            n [type="{bad_type}", timeout=120]
            start -> n -> exit
        }}
    """))
    violations = graph_audit.audit_graph(p)
    flagged = [
        v for v in violations
        if v.kind in ("G3", "R1") and v.location == "n"
    ]
    assert len(flagged) == 1, (
        f"expected 1 G3/R1 violation for type={bad_type!r}; got {len(flagged)}: "
        f"{[(v.kind, v.location, v.message) for v in flagged]}"
    )
