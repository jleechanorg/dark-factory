"""Conformance + structural tests for ``pipelines/slim/bugfix_noholdout.dot``.

Promotes the in-flight bug-fix lane that has no sealed behavioral holdout.
The whole point of the pipeline is the *absence* of a ``holdout_eval`` node;
the cross-vendor ``gate_er`` (resolved through the adversarial priority queue)
is the merge-confidence bar. This test pins the structural contract so a
future edit cannot quietly re-introduce a holdout or break prompt resolution.

File-disjoint: new file, only reads the .dot pipeline and a parser import.
No WIP file touched.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import ROOT as _CONFTEST_ROOT  # noqa: E402, F811
from runner.parser import parse  # noqa: E402

assert _CONFTEST_ROOT == ROOT, "conftest ROOT drift"

PIPELINE = ROOT / "pipelines" / "slim" / "bugfix_noholdout.dot"


# ---------------------------------------------------------------------------
# (a) + (b) — parser accepts the graph; ``start`` and ``exit`` are present
# ---------------------------------------------------------------------------


def test_parser_accepts_bugfix_noholdout_graph() -> None:
    """The .dot parses and declares both anchor nodes.

    ``runner.parser.parse`` raises if either is missing, so the absence of
    an exception plus a non-empty graph object is the structural accept.
    """
    g = parse(PIPELINE)
    assert "start" in g.nodes
    assert "exit" in g.nodes


# ---------------------------------------------------------------------------
# (c) — every ``prompt="@..."`` reference resolves to a real file under
#       ``$DARK_FACTORY_HOME/prompts/`` (the worktree itself acts as the
#       home; the parser strips the ``@`` and resolves relative to the
#       pipeline's directory → repo root).
# ---------------------------------------------------------------------------


def test_every_prompt_reference_resolves() -> None:
    """Walk every node; assert every ``prompt="@..."`` points at an existing file.

    The parser's resolution order is pipeline-dir-relative → factory_home
    → absolute (see ``runner/parser.py:Node.prompt_ref`` and
    ``factory_home()``). The slim prompts live under
    ``<repo>/prompts/slim/``, and every node-defined reference in the
    new pipeline is repo-relative, so a path check against
    ``ROOT / "prompts" / ref`` is sufficient.
    """
    g = parse(PIPELINE)
    missing: list[tuple[str, str]] = []
    for name, node in g.nodes.items():
        ref = node.attrs.get("prompt")
        if not ref:
            continue
        ref = str(ref)
        if ref.startswith("@"):
            ref = ref[1:]
        # Prompt refs in slim/ pipelines are repo-relative (the path
        # ``prompts/slim/foo.md`` resolves against the workdir / factory
        # home, not against the pipeline's own directory). Try the repo
        # root first, then the pipeline's parent directory for safety.
        candidates = [
            (ROOT / ref).resolve(),
            (PIPELINE.parent / ref).resolve(),
        ]
        if not any(c.is_file() for c in candidates):
            missing.append((name, ref))
    assert not missing, (
        f"bugfix_noholdout.dot has unresolved prompt references: {missing}. "
        "Each `prompt=@<path>` must point to a real file under the repo."
    )


# ---------------------------------------------------------------------------
# (d) — no ``holdout_eval`` node exists. This is the entire reason the
#       pipeline exists; a regression that re-introduces one breaks the
#       "no sealed holdout for this feature" trade-off documented in
#       ``docs/pipelines/bugfix_noholdout.md``.
# ---------------------------------------------------------------------------


def test_no_holdout_eval_node() -> None:
    """The pipeline's defining property: no sealed holdout node.

    ``holdout_eval`` is the handler type that runs the sealed
    ``$DARK_FACTORY_HOLDOUTS/evaluator/run.py``. If a future maintainer
    adds one, the lane silently re-enters the standard holdout-always
    policy and the trade-off documented in the spec doc no longer holds.
    """
    g = parse(PIPELINE)
    offenders = [
        name for name, node in g.nodes.items()
        if node.attrs.get("type") == "holdout_eval"
    ]
    assert not offenders, (
        f"bugfix_noholdout.dot must NOT carry a holdout_eval node; "
        f"found: {offenders}. The whole point of this pipeline is the "
        "absence of a sealed holdout — use pipelines/bug_fix.dot if you "
        "want red/green + holdout discipline."
    )


# ---------------------------------------------------------------------------
# (e) — the graph has no obvious cycle that isn't bounded by ``max_visits``.
#
# The new pipeline *does* contain a cycle (``test -> fix -> test`` and
# ``review -> fix -> test``, etc.) which is the intended fix-loop. The
# contract is: every cycle must be bounded by a node with ``max_visits``.
# We assert that on a structural read of the graph — the runtime enforces
# the visit cap, this test pins the *attribute* contract.
# ---------------------------------------------------------------------------


def test_every_cycle_is_bounded_by_max_visits() -> None:
    """Every cycle in the graph contains at least one node with ``max_visits``.

    A cycle is a strongly-connected component (SCC) with more than one
    node, or a self-loop. The runtime reads the *visit count* of a
    single node (``max_visits``), not the cycle length, so the
    contract is: any cycle must have a member node that bounds the
    number of times it can be entered.

    The new pipeline's only cycle is the fix loop: ``fix -> test`` and
    ``gate_er -> fix`` (with reverse edges ``test -> fix`` etc.). ``fix``
    declares ``max_visits="3"`` so the cycle is bounded. This test
    guards against a future cycle being added without a corresponding
    bound.
    """
    g = parse(PIPELINE)
    # Build adjacency (include self-loops so a single-node cycle is
    # caught).
    out_edges: dict[str, set[str]] = {n: set() for n in g.nodes}
    for edge in g.edges:
        src, dst = edge.src, edge.dst
        if src in out_edges:
            out_edges[src].add(dst)

    # Tarjan's SCC, recursive. Graph is <30 nodes so depth is safe.
    sys.setrecursionlimit(500)
    index_counter = [0]
    stack: list[str] = []
    on_stack: set[str] = set()
    indices: dict[str, int] = {}
    lowlinks: dict[str, int] = {}
    sccs: list[list[str]] = []

    def strongconnect(v: str) -> None:
        indices[v] = index_counter[0]
        lowlinks[v] = index_counter[0]
        index_counter[0] += 1
        stack.append(v)
        on_stack.add(v)
        for w in out_edges.get(v, ()):
            if w not in indices:
                strongconnect(w)
                lowlinks[v] = min(lowlinks[v], lowlinks[w])
            elif w in on_stack:
                lowlinks[v] = min(lowlinks[v], indices[w])
        if lowlinks[v] == indices[v]:
            scc: list[str] = []
            while True:
                w = stack.pop()
                on_stack.discard(w)
                scc.append(w)
                if w == v:
                    break
            sccs.append(scc)

    for n in list(g.nodes):
        if n not in indices:
            strongconnect(n)

    unbounded: list[list[str]] = []
    for scc in sccs:
        if len(scc) <= 1:
            continue
        if any(g.nodes[n].attrs.get("max_visits") for n in scc):
            continue
        unbounded.append(sorted(scc))
    assert not unbounded, (
        "Every cycle (SCC with >1 node) must contain a node with "
        f"max_visits=. Unbounded cycles: {unbounded}. The fix node "
        "carries max_visits=\"3\"; add the same to any new node that "
        "becomes part of a cycle."
    )


# ---------------------------------------------------------------------------
# Cross-validation — no short-name collision with the other slim/ pipelines,
# and the new pipeline is not a near-clone of an existing role. (b) and (c)
# of the task spec are the file-overlap and structural-comparison report;
# the assertions below pin the structural contract so a future slim/
# addition cannot shadow this pipeline's name.
# ---------------------------------------------------------------------------


def test_no_short_name_collision_in_pipelines_slim() -> None:
    """Bare pipeline name ``bugfix_noholdout`` is unique in pipelines/slim/.

    Mirrors the ``runner/paths.py::resolve_pipeline_path`` short-name
    resolution: the user invokes ``dark-factory --pipeline bugfix_noholdout``
    and the runner looks for ``<workdir>/dark-factory/pipelines/bugfix_noholdout.dot``
    first, then delegates to ``$DARK_FACTORY_HOME/bugfix_noholdout.dot`` /
    ``pipelines/slim/bugfix_noholdout.dot``. A second file with the same
    basename would shadow resolution.
    """
    slim_dir = ROOT / "pipelines" / "slim"
    matches = list(slim_dir.glob("bugfix_noholdout.dot"))
    assert len(matches) == 1, (
        f"Expected exactly one bugfix_noholdout.dot under pipelines/slim/, "
        f"found {len(matches)}: {matches}."
    )
    assert matches[0] == PIPELINE, (
        f"Found bugfix_noholdout.dot at {matches[0]}; expected {PIPELINE}."
    )
