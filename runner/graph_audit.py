"""Static structural audit for dark-factory ``.dot`` pipelines.

Detects two of the most expensive reviewer gaps in the G-tuple catalogued
in ``docs/factory-evolve-research/reviewer-gap-taxonomy.md``:

* **G1** — code-producing graph has no reviewer wired. A graph is
  G1-positive when there exists a path from ``start`` to ``exit`` that
  contains at least one code-producing node (resolved handler is
  ``_codergen``) and contains **zero** reviewer nodes (handler type is
  one of ``gate_es | gate_er | gate_code_standards | holdout_eval``).

* **G2** — reviewer routes ``outcome!=success`` to ``exit`` instead of a
  bounded fix loop. A graph is G2-positive when any edge ``reviewer ->
  exit`` carries a condition whose normalized form is
  ``outcome != success`` (or the equivalent ``outcome != "success"``).

This module is **pure metadata analysis**. No LLM calls, no NL
classification. Handler classification mirrors ``runner.handlers.resolve``
EXACTLY so a node we say is "code-producing" is what the engine would
actually invoke as ``_codergen`` at runtime. The structural analogy to
``runner/healer.py``'s prefix routing is intentional: CLAUDE.md ZFC
section exempts internal data routing over the engine's own node
namespace from the "no heuristic classification" rule.

The audit is invoked three ways:

1. ``audit_graph(path)`` — returns ``list[Violation]`` for a single .dot.
2. ``audit_graphs(dir)`` — walks ``dir.rglob("*.dot")`` and aggregates.
3. ``audit_repository(repo_root)`` — audits production graphs fully and
   applies universal route/parse checks to every authored benchmark graph.
4. ``python -m runner.graph_audit [pipeline_dir]`` — CLI; exit 0 if
   no violations, exit 1 + human-readable report otherwise. Pass
   ``--repository`` to select the repository-wide structural scope.
"""

from __future__ import annotations

import argparse
import pathlib
import sys
from dataclasses import dataclass
from typing import Iterable, Literal, Optional

from . import handlers
from .parser import (
    Edge,
    Graph,
    Node,
    _tokenize_condition,
    is_exit_node,
    is_start_node,
    parse,
)


# ---------------------------------------------------------------------------
# Constants mirrored from runner.handlers.{TYPE_REGISTRY,REGISTRY} and
# runner.parser.{is_start_node,is_exit_node}. We intentionally read the
# registries at runtime (instead of duplicating their contents) so the
# audit cannot drift from the engine's actual handler resolution order.
# ---------------------------------------------------------------------------

# Topology-only DOT shapes that never reach ``_codergen`` at runtime.
# Mirrors CLAUDE.md § "Handlers" and the comment in
# ``runner/handler_codergen.py``'s contract tests: a ``point`` node is a
# zero-width routing anchor; ``component`` and ``tripleoctagon`` are the
# Kilroy fan-out / fan-in shape aliases registered in REGISTRY.
TOPOLOGY_ONLY_SHAPES: frozenset[str] = frozenset({"point", "component", "tripleoctagon"})

# Reviewer node types — the G1 / G2 violation target. Matched against the
# node's resolved handler *name* so a node declared as
# ``type="gate_er"`` is a reviewer even if the implementation moves it
# between modules. Mirrors the keys in ``runner.handlers.TYPE_REGISTRY``
# that gate / holdout the workflow at the boundary between coder output
# and merge readiness.
_REVIEWER_TYPE_NAMES: frozenset[str] = frozenset(
    {"gate_es", "gate_er", "gate_skeptic", "gate_code_standards", "holdout_eval", "parallel_reviewer", "web_advice"}
)

# Hard-tier reviewer type names. A node declaring ``type=<one-of-these>``
# in a .dot MUST have its handler registered in
# ``runner.handlers.TYPE_REGISTRY`` or the runtime resolver falls through
# to ``_codergen`` (silent regression — gate_skeptic P0 bug class).
# Mirrors the bead-jleechan-0qy.5 contract: every hard-tier reviewer
# class has a real handler; missing handler = fail loudly here, not at
# runtime.
_HARD_TIER_REVIEWER_TYPES: frozenset[str] = frozenset({
    "gate_es",
    "gate_er",
    "gate_skeptic",
    "gate_code_standards",
    "holdout_eval",
    "parallel_reviewer",
})


# ---------------------------------------------------------------------------
# Advisory allowlist — pipelines that are exempt from the audit by
# design. Each entry is a repo-relative path under the CWD and the
# comment justifies *why* the graph is intentionally non-compliant.
# ---------------------------------------------------------------------------
ADVISORY_ALLOWLIST: set[str] = {
    # Smoke graph — has no codergen node at all, exits via a hello
    # workflow stub. Including it in the audit would force every
    # smoke pipeline to declare a reviewer, which the Attractor
    # benchmark explicitly forbids ("smoke must remain cheap").
    "pipelines/factory/hello.dot",
    # Parallel fan-out demo — topologically exhibits only fan-out /
    # fan-in nodes; the audit's G1 path finder would always flag it
    # because there are no codergen OR reviewer nodes by construction.
    "pipelines/parallel_demo.dot",
}


# ---------------------------------------------------------------------------
# Public types
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Violation:
    """A single structural audit finding.

    ``kind``        — "G1" (no reviewer wired) or "G2" (reviewer routes
                       failure to exit instead of a fix loop).
    ``pipeline``    — repo-relative POSIX path of the offending .dot.
    ``location``    — node-and-edge locator (e.g. "reviewer_node ->
                       exit") so a human can grep without re-parsing.
    ``message``     — human-readable explanation; one short line.
    """

    kind: Literal["G1", "G2", "G3", "G4", "G5", "R1"]
    pipeline: str
    location: str
    message: str


# ---------------------------------------------------------------------------
# Handler resolution — *must* match runner.handlers.resolve() exactly.
# We re-implement here (instead of calling ``resolve``) because
# ``resolve`` returns a callable, and we want a stable string label we
# can compare to ``_REVIEWER_TYPE_NAMES``. The order is identical to
# CLAUDE.md § "Handlers" and to runner.handlers.resolve():
#
#   1. explicit ``type=`` lookup in TYPE_REGISTRY
#   2. name == "start" / "exit"  →  built-ins
#   3. shape lookup in REGISTRY
#   4. default  →  ``_codergen``
# ---------------------------------------------------------------------------


def _resolved_type_label(node: Node) -> str:
    """Return the *type label* the engine would resolve this node to.

    The label is one of:

    * the string from ``node.attrs["type"]`` (if it resolves in
      ``TYPE_REGISTRY``),
    * ``"start"`` / ``"exit"`` for the built-in name/shape anchors,
    * ``"conditional"`` / ``"parallel"`` / ``"join"`` for shape-only
      registrations,
    * ``"codergen"`` for the default fallback (any other node without
      an explicit ``type`` that the engine would run as a coder).

    For reviewer detection we only need to know whether the label is
    one of ``_REVIEWER_TYPE_NAMES``; for code-producing detection we
    only need to know whether the label is ``"codergen"``. Both
    classifications come from the SAME resolution path so they
    cannot drift apart.
    """
    t = node.attrs.get("type")
    if t and t in handlers.TYPE_REGISTRY:
        return str(t)
    if is_start_node(node):
        return "start"
    if is_exit_node(node):
        return "exit"
    shape = node.shape
    if shape in handlers.REGISTRY:
        return str(shape)
    return "codergen"


def _is_code_producing(node: Node) -> bool:
    """A node is code-producing iff the engine would invoke ``_codergen``.

    Topology-only shapes never reach ``_codergen`` — they are routing
    aliases registered in ``REGISTRY``. Nodes with an explicit
    ``type=`` that resolves in ``TYPE_REGISTRY`` (gate_*, tool,
    conditional, holdout_eval, parallel, join) are *not* code
    producers because the engine dispatches them elsewhere.
    """
    if node.name in {"start", "exit"}:
        return False
    if node.shape in TOPOLOGY_ONLY_SHAPES:
        return False
    return _resolved_type_label(node) == "codergen"


def _is_reviewer(node: Node) -> bool:
    """A node is a reviewer iff its resolved type is in the reviewer set.

    Includes both ``type="gate_*"`` / ``type="holdout_eval"`` AND any
    future aliasing added to ``TYPE_REGISTRY`` under those names. Does
    NOT include ``type="codergen"`` even if the node is ``class="review"`` —
    the engine never resolves a ``class=review`` codergen to a reviewer
    handler; the class is a styling hint, not a dispatch contract.
    """
    return _resolved_type_label(node) in _REVIEWER_TYPE_NAMES


_CLAUDE_ROUTE_TOKENS: frozenset[str] = frozenset({"claude", "claude-sonnet"})


def _is_direct_claude_route(node: Node) -> bool:
    """Return whether ``node`` statically selects a Claude execution lane.

    Only exact backend/model tokens count.  ``model_name`` is deliberately
    ignored because it can name a model on a non-Claude backend.  Gate
    priorities are comma-separated backend tokens; substring lookalikes such
    as ``claudem`` and ``claude-sonnet-4-6`` are not direct routes.
    """
    resolved = _resolved_type_label(node)
    if resolved == "web_advice":
        return True
    # ``backend``/``model`` are explicit route selectors even when the node
    # resolves to a non-codergen handler (for example a gate or tool node).
    # Inspect them before handler-specific routing so every direct Claude
    # capability is held to the same scope-marker contract.
    for key in ("backend", "model"):
        token = str(node.attrs.get(key) or "").strip().lower()
        if token in _CLAUDE_ROUTE_TOKENS:
            return True
    if resolved == "codergen":
        return False
    priority = str(node.attrs.get("backend_priority") or "")
    tokens = {part.strip().lower() for part in priority.split(",") if part.strip()}
    return bool(tokens & _CLAUDE_ROUTE_TOKENS)


def _has_claude_scope_markers(node: Node) -> bool:
    """Return whether both explicit Claude-scope markers are truthy."""
    def truthy(value: object) -> bool:
        if isinstance(value, bool):
            return value
        return str(value or "").strip().lower() in {"1", "true", "yes", "on"}

    return truthy(node.attrs.get("explicit_claude_lane")) and truthy(
        node.attrs.get("requires_claude_config")
    )


# ---------------------------------------------------------------------------
# Condition normalisation — replicate parser._validate_condition's
# token-shape check so we can ask "is this edge conditioned on
# ``outcome != success``?" without re-implementing the whole grammar.
# ---------------------------------------------------------------------------


def _is_outcome_failure_condition(condition: Optional[str]) -> bool:
    """Return True iff ``condition`` tokenises to ``outcome != success``.

    Mirrors ``runner.parser._validate_condition``'s grammar: an
    ``EQ``/``NEQ`` comparison between the literal ``outcome`` and the
    literal ``success`` (with optional whitespace). Surrounding ``AND``
    / ``OR`` clauses would make this a complex condition; we treat
    those as NOT a simple failure routing, matching what the engine
    actually evaluates as "the failure branch".
    """
    if not condition:
        return False
    tokens = _tokenize_condition(condition)
    if tokens is None:
        return False
    # Drop leading/trailing NOT in the simple form.
    cleaned: list[tuple[str, str]] = []
    for kind, value in tokens:
        if kind == "SPACE":
            continue
        cleaned.append((kind, value))
    if len(cleaned) == 3:
        word_kind, word_val, op_kind, op_val, rhs_kind, rhs_val = (
            cleaned[0][0], cleaned[0][1],
            cleaned[1][0], cleaned[1][1],
            cleaned[2][0], cleaned[2][1],
        )
        if (
            word_kind == "WORD" and word_val == "outcome"
            and op_kind == "NEQ" and op_val == "!="
            and rhs_val == "success"
            and rhs_kind in ("WORD", "STRING")
        ):
            return True
    return False


# ---------------------------------------------------------------------------
# G1: path-without-reviewer detection.
#
# We BFS from ``start`` over outgoing edges, keeping the visited
# reviewer-set per path. We only care whether *any* path from start to
# exit visits a code-producer AND zero reviewers. Implemented as a
# recursive DFS with a ``visited + reviewer_seen`` state so we don't
# re-explore branches that already passed through a reviewer.
# ---------------------------------------------------------------------------


def _find_unregistered_handler_refs(graph: Graph) -> list[tuple[str, str]]:
    """Return ``[(node_name, type_name), ...]`` for every node whose
    explicit ``type="..."`` attribute references a handler NOT in
    ``runner.handlers.TYPE_REGISTRY`` and NOT resolved by shape/name
    fallbacks.

    Mirrors ``_resolved_type_label``'s contract. A node with NO explicit
    ``type=`` attribute resolves via shape/name/default and is always
    OK. Only nodes that EXPLICITLY say ``type="foo"`` where ``foo`` is
    not in TYPE_REGISTRY and not shape-resolvable are flagged.
    """
    bad: list[tuple[str, str]] = []
    for name, node in graph.nodes.items():
        if name in {"start", "exit"}:
            continue
        t = node.attrs.get("type")
        if not t:
            continue  # no explicit type → shape/name/default → always resolves
        if t in handlers.TYPE_REGISTRY:
            continue
        if node.shape in handlers.REGISTRY:
            continue
        bad.append((name, str(t)))
    return bad


def _find_unconfigured_holdout_refs(graph: Graph) -> list[str]:
    """Return holdout_eval nodes that do not declare how to find a feature."""
    bad: list[str] = []
    for name, node in graph.nodes.items():
        if _resolved_type_label(node) != "holdout_eval":
            continue
        feature = str(node.attrs.get("feature") or "").strip()
        if feature and (feature == "${state.feature}" or "${state.feature}" not in feature):
            continue
        if str(node.attrs.get("allow_missing_feature") or "").lower() == "true":
            continue
        bad.append(name)
    return bad


def _find_unreviewed_code_paths(graph: Graph) -> list[tuple[str, ...]]:
    """Return one witness path per ``start -> exit`` branch that
    contains a code-producer and zero reviewer nodes.

    The path is a tuple of node names ``(start, ..., exit)``. Multiple
    witnesses can be returned if multiple branches qualify. Cycles are
    bounded by not re-entering a node already on the current path.
    """
    start = _find_start_node(graph)
    if start is None:
        return []
    out: list[tuple[str, ...]] = []
    _dfs_witness(graph, start, (), False, out)
    return out


def _find_start_node(graph: Graph) -> Optional[str]:
    for name, node in graph.nodes.items():
        if is_start_node(node):
            return name
    return None


def _is_failure_terminal_node(node: Node) -> bool:
    """Return whether ``node`` terminates a non-success branch.

    ``exit`` is the only terminal recognized by the graph engine.  Do not
    infer terminal semantics from arbitrary DOT attributes such as
    ``failure_terminal`` or ``terminal="failure"``: those attributes have no
    runtime meaning and could hide a continuation stage from G1's witness
    search.  Keeping this predicate separate makes the edge lookup safe and
    ensures continuation targets (for example ``implement`` or ``review``)
    are never mistaken for terminal failure handling.
    """
    return is_exit_node(node)


def _dfs_witness(
    graph: Graph,
    current: str,
    path: tuple[str, ...],
    saw_reviewer: bool,
    out: list[tuple[str, ...]],
) -> None:
    new_path = path + (current,)
    node = graph.nodes[current]
    saw_code = any(
        _is_code_producing(graph.nodes[n]) for n in new_path
    )
    current_saw_reviewer = saw_reviewer or _is_reviewer(node)
    outgoing = graph.outgoing(current)
    if not outgoing:
        # Dead-end (non-exit) leaf — not a witness unless it is exit.
        if is_exit_node(node) and saw_code and not current_saw_reviewer:
            out.append(new_path)
        return
    for edge in outgoing:
        dst = edge.dst
        dst_node = graph.nodes.get(dst)
        if dst_node is None:
            # Be defensive for hand-built Graph values; parsed DOT graphs
            # reject unknown destinations before reaching the audit.
            continue
        # Cycle / re-visit guard: do not descend into a node already on
        # the current path. (max_visits-style bounds are runtime
        # concerns; here we only need to avoid infinite recursion.)
        if dst in new_path:
            if is_exit_node(dst_node) and saw_code and not current_saw_reviewer:
                out.append(new_path + (dst,))
            continue
        # Failure-terminal edges are intentionally excluded from the G1
        # happy-path search. A planner/backend failure may route to an
        # explicit terminal sink so the engine preserves a non-success
        # result; that is not an unreviewed *successful* merge path. G2
        # separately audits reviewer failure -> exit edges, so skipping these
        # here does not weaken the review-failure contract. Non-success edges
        # into continuation stages remain part of the audited path search.
        if (
            _is_outcome_failure_condition(edge.condition)
            and _is_failure_terminal_node(dst_node)
        ):
            continue
        _dfs_witness(graph, dst, new_path, current_saw_reviewer, out)


# ---------------------------------------------------------------------------
# G2: reviewer -> exit with outcome!=success.
# ---------------------------------------------------------------------------


def _find_g2_violations(graph: Graph, relpath: str) -> list[Violation]:
    violations: list[Violation] = []
    for edge in graph.edges:
        src_node = graph.nodes.get(edge.src)
        if src_node is None or not _is_reviewer(src_node):
            continue
        dst_node = graph.nodes.get(edge.dst)
        if dst_node is None or not is_exit_node(dst_node):
            continue
        if _is_outcome_failure_condition(edge.condition):
            violations.append(
                Violation(
                    kind="G2",
                    pipeline=relpath,
                    location=f"{edge.src} -> {edge.dst}",
                    message=(
                        f"reviewer node {edge.src!r} routes "
                        f"outcome!=success directly to exit; failure "
                        f"verdicts should loop back to a bounded fix "
                        f"node, not exit"
                    ),
                )
            )
    return violations


# ---------------------------------------------------------------------------
# Public audit entrypoints
# ---------------------------------------------------------------------------


def audit_graph(
    path: pathlib.Path, *, repo_root: Optional[pathlib.Path] = None
) -> list[Violation]:
    """Audit a single ``.dot`` pipeline.

    Library files (those that ``include="@..."`` other graphs and
    declare no ``start``/``exit`` of their own — e.g.
    ``pipelines/_base.dot``) are skipped silently: they are not
    runnable, so the G1/G2 contract does not apply to them. Per the
    spec the scanner walks them; it just does not report violations.

    Parse errors on non-library files are surfaced as a synthetic
    violation so a downstream driver can still report the file as
    broken.
    """
    # Repository-wide callers pass their root explicitly so report paths do
    # not depend on the process CWD.  Standalone callers retain the historical
    # CWD-relative behavior when no root is supplied.
    relpath = _relpath(path, repo_root)
    if relpath in ADVISORY_ALLOWLIST:
        return []
    try:
        graph = parse(path, repo_root=repo_root)
    except ValueError as exc:
        # A "must contain both 'start' and 'exit' nodes" error on a
        # file that has an `include` graph-attribute is a *library*,
        # not a broken pipeline. The library contract is enforced by
        # ``_resolve_includes``; scanning the bare library file
        # produces the same ValueError regardless of contents. Skip
        # such files silently — but ONLY when the raw file actually
        # carries the library-defining ``include="@..."`` graph
        # attribute. Otherwise the parse failure is real and the
        # author needs to see the violation.
        if "start" in str(exc) and "exit" in str(exc):
            try:
                raw = path.read_text(encoding="utf-8")
            except OSError:
                raw = ""
            if 'include="@' in raw:
                return []
        return [
            Violation(
                kind="G1",
                pipeline=relpath,
                location="<parse>",
                message=f"pipeline failed to parse: {type(exc).__name__}: {exc}",
            )
        ]
    except Exception as exc:
        return [
            Violation(
                kind="G1",
                pipeline=relpath,
                location="<parse>",
                message=f"pipeline failed to parse: {type(exc).__name__}: {exc}",
            )
        ]

    violations: list[Violation] = []

    # G5 — every statically direct Claude route must be explicitly scoped.
    # The markers are node-local so a graph-level/default backend cannot make
    # an implicit route appear authorized.
    for node_name, node in graph.nodes.items():
        if _is_direct_claude_route(node) and not _has_claude_scope_markers(node):
            violations.append(
                Violation(
                    kind="G5",
                    pipeline=relpath,
                    location=node_name,
                    message=(
                        f"node {node_name!r} directly routes to Claude but is missing "
                        "explicit_claude_lane=true and requires_claude_config=true"
                    ),
                )
            )

    # G3 / R1 — node explicitly references a handler type the runner
    # cannot resolve. Two flavors:
    #   (a) hard-tier reviewer types not registered (gate_skeptic P0
    #       bug class) — emits "G3";
    #   (b) any other type-typo (gate_erx, parallel_reviwer, etc.)
    #       that would silently fall through to _codergen — emits "R1".
    unresolved = _find_unregistered_handler_refs(graph)
    for node_name, type_name in unresolved:
        is_hard_tier = type_name in _HARD_TIER_REVIEWER_TYPES
        violations.append(
            Violation(
                kind="G3" if is_hard_tier else "R1",
                pipeline=relpath,
                location=node_name,
                message=(
                    f"node {node_name!r} declares type={type_name!r} but "
                    f"{type_name!r} is not in runner.handlers.TYPE_REGISTRY. "
                    f"Runtime resolver falls through to _codergen "
                    f"(silent regression)"
                    + (" — hard-tier reviewer MUST be registered"
                       if is_hard_tier else "")
                ),
            )
        )

    for node_name in _find_unconfigured_holdout_refs(graph):
        violations.append(
            Violation(
                kind="G4",
                pipeline=relpath,
                location=node_name,
                message=(
                    f"holdout_eval node {node_name!r} has no feature. Set "
                    'feature="<holdout-feature>" or feature="${state.feature}", '
                    'or add allow_missing_feature="true" for an intentional '
                    "holdout-less/externally configured path."
                ),
            )
        )

    # G1 — at least one path from start to exit that visits a
    # code-producer and zero reviewers.
    code_producers = [n for n in graph.nodes.values() if _is_code_producing(n)]
    if code_producers:
        unreviewed = _find_unreviewed_code_paths(graph)
        if unreviewed:
            # One violation per witness path keeps the message
            # actionable; collapse them if they share a code-producer
            # to avoid spam when the same graph has many branches.
            for witness in unreviewed:
                producer_names = [
                    n for n in witness if _is_code_producing(graph.nodes[n])
                ]
                producer = producer_names[0] if producer_names else "?"
                violations.append(
                    Violation(
                        kind="G1",
                        pipeline=relpath,
                        location=" -> ".join(witness),
                        message=(
                            f"code-producing node {producer!r} has a path "
                            f"to exit with no reviewer node (G1: graph "
                            f"reaches exit un-reviewed)"
                        ),
                    )
                )

    # G2 — reviewer -> exit on outcome!=success.
    violations.extend(_find_g2_violations(graph, relpath))

    return violations


def audit_graphs(pipeline_dir: pathlib.Path) -> list[Violation]:
    """Walk ``pipeline_dir.rglob('*.dot')`` and audit every .dot found.

    Subdirs (``pipelines/slim/``, ``pipelines/factory/``) and
    include-fragments (``pipelines/_base.dot``) are all reached by
    ``rglob``. The order is sorted so reports are deterministic across
    filesystems.
    """
    if not pipeline_dir.exists():
        raise FileNotFoundError(f"pipeline dir does not exist: {pipeline_dir}")
    violations: list[Violation] = []
    for dot in sorted(pipeline_dir.rglob("*.dot")):
        violations.extend(audit_graph(dot))
    return violations


def _is_authored_graph(repo_root: pathlib.Path, path: pathlib.Path) -> bool:
    """Return whether ``path`` is an authored repository pipeline graph.

Production graphs live below top-level ``pipelines/`` and ``.dark-factory/``
    directories, while benchmark graphs either use that conventional directory
    or are a direct ``.dot`` file below ``benchmarks/<name>/``. Generated
    evidence and test fixtures intentionally sit outside those structural roots
    and are not shipped pipeline inputs.
    """
    try:
        relative = path.resolve().relative_to(repo_root.resolve())
    except ValueError:
        return False
    parts = relative.parts
    return bool(parts) and (
        parts[0] in {"pipelines", ".dark-factory"} or parts[0] == "benchmarks"
    )


def audit_repository(repo_root: pathlib.Path) -> list[Violation]:
    """Audit every authored graph discoverable in a repository tree.

    Unlike ``audit_graphs(pipelines/)``, this boundary includes nested
    benchmark pipeline families, hidden ``.dark-factory/`` slice graphs, and
    the production graph root. The
    production root receives the complete G1-G5 audit. Benchmark graphs are
    checked for the cross-cutting direct-Claude scope invariant (G5) and
    parse failures, while their intentionally varied reviewer topologies are
    not reclassified as production G1/G4 failures. Discovery is structural
    (repository source roots and ``*.dot`` files), so adding a new benchmark
    does not require updating an allowlist.
    """
    if not repo_root.exists():
        raise FileNotFoundError(f"repository root does not exist: {repo_root}")
    if not repo_root.is_dir():
        raise NotADirectoryError(f"repository root is not a directory: {repo_root}")
    violations: list[Violation] = []
    for dot in sorted(repo_root.rglob("*.dot")):
        if _is_authored_graph(repo_root, dot):
            # ``audit_graph`` normally reports relative to CWD.  A repository
            # audit may be invoked from any directory, so bind its report
            # paths to the repository root explicitly.
            graph_violations = audit_graph(dot, repo_root=repo_root)
            try:
                relative = dot.resolve().relative_to(repo_root.resolve())
            except ValueError:
                continue
            if relative.parts[0] == "benchmarks":
                # G5 is a universal route-scope invariant. Parse errors are
                # also universally actionable, regardless of graph family.
                violations.extend(
                    v for v in graph_violations
                    if v.kind == "G5" or v.location == "<parse>"
                )
            else:
                violations.extend(graph_violations)
    return violations


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _relpath(
    path: pathlib.Path, root: Optional[pathlib.Path] = None
) -> str:
    """Return a stable POSIX report path relative to ``root`` or CWD.

    Repository audits supply ``root`` explicitly because the caller may be
    outside the repository.  Standalone graph audits retain CWD-relative
    behavior and fall back to the path's spelling when it is external.
    """
    base = root.resolve() if root is not None else pathlib.Path.cwd()
    try:
        return path.resolve().relative_to(base).as_posix()
    except ValueError:
        return path.as_posix()


def _format_report(violations: Iterable[Violation]) -> str:
    lines = []
    for v in violations:
        lines.append(f"{v.kind} | {v.pipeline} | {v.location} | {v.message}")
    return "\n".join(lines)


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m runner.graph_audit",
        description=(
            "Static structural audit for dark-factory pipelines. "
            "Exits 0 if no graph-audit violations are found under the "
            "given directory, 1 otherwise."
        ),
    )
    parser.add_argument(
        "pipeline_dir",
        nargs="?",
        default="pipelines",
        help="Root directory to walk for *.dot files (default: ./pipelines).",
    )
    parser.add_argument(
        "--repository",
        action="store_true",
        help="Discover and audit authored graphs under both pipelines/ and benchmarks/.",
    )
    args = parser.parse_args(argv)

    root = pathlib.Path(args.pipeline_dir)
    try:
        violations = audit_repository(root) if args.repository else audit_graphs(root)
    except FileNotFoundError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    if not violations:
        print("OK: no graph-audit violations found.")
        return 0

    print(_format_report(violations))
    return 1


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
