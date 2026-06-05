"""Graph intermediate representation (graph-IR) for workflow_graphgen.

This is the generator output contract (spec §"Generator output contract"): a
description object with five fields — ``nodes``, ``edges``, ``guaranteed``,
``rationale``, ``vocabulary``. The same IR is generated once per goal and shared
by both Mode A and Mode A+B (fairness control), so the dynamic middle entering
each mode is byte-identical.

The IR describes only the **dynamic middle** work nodes plus the names of the
pinned guaranteed reviewers. ``start``/``exit`` and the concrete terminal
reviewer tail are injected by the renderer, identically for both modes, so the
fairness invariants (terminal reviewers, no goal_gate, no retry_target) cannot be
perturbed by the generator.
"""

from __future__ import annotations

import pathlib
from dataclasses import dataclass, field

from .catalog import VOCABULARY, load_catalog

# Canonical pinned reviewer tail, identical in both modes. Each is TERMINAL:
# goal_gate is unset and the onward edge is unconditional, so a failing verdict is
# recorded and the run proceeds to exit — never back to a fix node. goal_gate is
# deliberately absent because the engine treats goal_gate=true as the
# retry-on-failure trigger (engine.py:_goal_gate_target), which is exactly the
# A-vs-A+B asymmetry the spec forbids.
GUARANTEED_NODES = ("code_reviewer", "gate_es", "gate_er")

# Independent cold code reviewer: a fresh `claude --print /code_standards`
# process that never saw the coder prompt and sees only the committed diff + repo
# state (Attractor guarantee). The codex variant is pluggable but `codex exec`
# proved flaky on long prompts (see project memory), so the default uses the
# equivalent independent claude gate per CLAUDE.md tenet 3 ("codex exec ... or
# equivalent").
_GUARANTEED_TAIL_TYPES = {
    "code_reviewer": "gate_code_standards",
    "gate_es": "gate_es",
    "gate_er": "gate_er",
}


@dataclass
class NodeIR:
    name: str
    type: str  # one of VOCABULARY
    prompt: str  # catalog-relative path (no leading '@')
    backend: str | None = None
    model_name: str | None = None


@dataclass
class EdgeIR:
    src: str
    dst: str
    condition: str | None = None


@dataclass
class GraphIR:
    nodes: list[NodeIR]
    edges: list[EdgeIR]
    guaranteed: list[str] = field(default_factory=lambda: list(GUARANTEED_NODES))
    rationale: str = ""
    vocabulary: list[str] = field(default_factory=lambda: list(VOCABULARY))

    # ---- contract (de)serialization ------------------------------------
    @classmethod
    def from_dict(cls, d: dict) -> "GraphIR":
        nodes = [
            NodeIR(
                name=n["name"],
                type=n["type"],
                prompt=n["prompt"],
                backend=n.get("backend"),
                model_name=n.get("model_name"),
            )
            for n in d["nodes"]
        ]
        edges = [
            # Accept both spec-documented {from, to} and internal {src, dst} forms.
            EdgeIR(
                src=e.get("src") or e["from"],
                dst=e.get("dst") or e["to"],
                condition=e.get("condition"),
            )
            for e in d["edges"]
        ]
        return cls(
            nodes=nodes,
            edges=edges,
            guaranteed=list(d.get("guaranteed", GUARANTEED_NODES)),
            rationale=d.get("rationale", ""),
            vocabulary=list(d.get("vocabulary", VOCABULARY)),
        )

    def to_dict(self) -> dict:
        return {
            "nodes": [
                {
                    k: v
                    for k, v in {
                        "name": n.name,
                        "type": n.type,
                        "prompt": n.prompt,
                        "backend": n.backend,
                        "model_name": n.model_name,
                    }.items()
                    if v is not None
                }
                for n in self.nodes
            ],
            "edges": [
                {k: v for k, v in {"src": e.src, "dst": e.dst, "condition": e.condition}.items() if v is not None}
                for e in self.edges
            ],
            "guaranteed": list(self.guaranteed),
            "rationale": self.rationale,
            "vocabulary": list(self.vocabulary),
        }

    # ---- structural validation -----------------------------------------
    def validate(self) -> None:
        """Validate the IR against the allowed vocabulary, catalog, and edge
        well-formedness. Raises ValueError on the first problem found."""
        if not self.nodes:
            raise ValueError("graph-IR has no dynamic-middle nodes")
        catalog = load_catalog()["prompts"]
        names = [n.name for n in self.nodes]
        if len(names) != len(set(names)):
            raise ValueError(f"duplicate middle node names: {names}")
        reserved = set(GUARANTEED_NODES) | {"start", "exit"}
        for n in self.nodes:
            if n.type not in VOCABULARY:
                raise ValueError(f"node {n.name!r} uses type {n.type!r} outside vocabulary")
            if n.name in reserved:
                raise ValueError(f"middle node {n.name!r} collides with a reserved/guaranteed name")
            ref = n.prompt[1:] if n.prompt.startswith("@") else n.prompt
            expected = catalog.get(n.type)
            if expected is None:
                raise ValueError(f"node type {n.type!r} not in catalog")
            if ref != expected:
                raise ValueError(
                    f"node {n.name!r} (type={n.type!r}) prompt {n.prompt!r} "
                    f"does not match catalog entry {expected!r}"
                )
        if list(self.guaranteed) != list(GUARANTEED_NODES):
            raise ValueError(
                f"guaranteed must be the pinned set {GUARANTEED_NODES}, got {self.guaranteed}"
            )
        # Middle edges must reference middle nodes only (start/exit/reviewers are
        # wired by the renderer).
        middle = set(names)
        for e in self.edges:
            if e.src not in middle or e.dst not in middle:
                raise ValueError(f"middle edge {e.src}->{e.dst} references a non-middle node")

    def middle_chain(self) -> list[NodeIR]:
        """Return middle nodes in execution order.

        Order is taken from the IR's edge list when it forms a simple linear
        chain (the smoke generator emits linear middles); otherwise falls back to
        declaration order. Mode A+B walks this exact order so its middle matches
        Mode A's runner walk.
        """
        if not self.edges:
            return list(self.nodes)
        by_name = {n.name: n for n in self.nodes}
        succ = {e.src: e.dst for e in self.edges if e.condition is None}
        has_pred = {e.dst for e in self.edges}
        roots = [n.name for n in self.nodes if n.name not in has_pred]
        if len(roots) != 1 or len(succ) != len(self.nodes) - 1:
            return list(self.nodes)  # not a simple chain; preserve declaration order
        ordered, cur, seen = [], roots[0], set()
        while cur is not None and cur not in seen:
            seen.add(cur)
            ordered.append(by_name[cur])
            cur = succ.get(cur)
        if len(ordered) != len(self.nodes):
            return list(self.nodes)
        return ordered


# ---- rendering ---------------------------------------------------------

def _q(value: str) -> str:
    return '"' + str(value).replace('"', '\\"') + '"'


def _render_middle_node(n: NodeIR) -> str:
    attrs = [f'type={_q(n.type)}', f'label={_q(n.name)}', f'prompt={_q("@" + (n.prompt[1:] if n.prompt.startswith("@") else n.prompt))}']
    if n.backend:
        attrs.append(f'backend={_q(n.backend)}')
    if n.model_name:
        attrs.append(f'model_name={_q(n.model_name)}')
    return f'    {n.name} [{", ".join(attrs)}]'


def _render_reviewer_tail_nodes() -> list[str]:
    lines = []
    for name in GUARANTEED_NODES:
        ntype = _GUARANTEED_TAIL_TYPES[name]
        lines.append(f'    {name} [type={_q(ntype)}, label={_q(name)}]')
    return lines


def _reviewer_tail_edges(entry_from: str) -> list[str]:
    """Unconditional terminal chain: entry_from -> code_reviewer -> gate_es ->
    gate_er -> exit. No conditions, no goal_gate, no retry_target."""
    chain = [entry_from, *GUARANTEED_NODES, "exit"]
    return [f"    {a} -> {b}" for a, b in zip(chain, chain[1:])]


def render_full_dot(ir: GraphIR, name: str = "wfgg_full") -> str:
    """Mode A graph: start -> <middle chain> -> reviewer tail -> exit.

    The middle is walked by the Python runner. Reviewer tail is terminal.
    """
    ir.validate()
    chain = ir.middle_chain()
    lines = [f"digraph {name} {{", '    graph [goal="workflow_graphgen mode A"]', "    rankdir=LR", ""]
    lines.append('    start [shape=Mdiamond, label="Start"]')
    lines.append('    exit  [shape=Msquare,  label="Exit"]')
    for n in chain:
        lines.append(_render_middle_node(n))
    lines.extend(_render_reviewer_tail_nodes())
    lines.append("")
    # start -> first middle -> ... -> last middle
    seq = ["start", *[n.name for n in chain]]
    for a, b in zip(seq, seq[1:]):
        lines.append(f"    {a} -> {b}")
    lines.extend(_reviewer_tail_edges(chain[-1].name))
    lines.append("}")
    return "\n".join(lines) + "\n"


def render_spine_dot(ir: GraphIR, name: str = "wfgg_spine") -> str:
    """Mode A+B spine: start -> reviewer tail -> exit (no middle).

    The middle ran in the harness and was committed before this spine runs, so
    the reviewers grade the committed diff through the same runner as Mode A.
    """
    ir.validate()
    lines = [f"digraph {name} {{", '    graph [goal="workflow_graphgen mode A+B spine"]', "    rankdir=LR", ""]
    lines.append('    start [shape=Mdiamond, label="Start"]')
    lines.append('    exit  [shape=Msquare,  label="Exit"]')
    lines.extend(_render_reviewer_tail_nodes())
    lines.append("")
    lines.extend(_reviewer_tail_edges("start"))
    lines.append("}")
    return "\n".join(lines) + "\n"


def render_middle_only_dot(ir: GraphIR, name: str = "wfgg_middle") -> str:
    """Mode A middle phase: start -> <middle chain> -> exit (no reviewers).

    Lets Mode A run the middle through the runner, commit, then run the SAME
    reviewer spine as Mode A+B — making 'who runs the middle' the sole variable.
    """
    ir.validate()
    chain = ir.middle_chain()
    lines = [f"digraph {name} {{", '    graph [goal="workflow_graphgen mode A middle"]', "    rankdir=LR", ""]
    lines.append('    start [shape=Mdiamond, label="Start"]')
    lines.append('    exit  [shape=Msquare,  label="Exit"]')
    for n in chain:
        lines.append(_render_middle_node(n))
    lines.append("")
    seq = ["start", *[n.name for n in chain], "exit"]
    for a, b in zip(seq, seq[1:]):
        lines.append(f"    {a} -> {b}")
    lines.append("}")
    return "\n".join(lines) + "\n"
