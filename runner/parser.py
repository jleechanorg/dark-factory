"""DOT → graph model.

Parses Graphviz DOT files produced for Attractor-pattern pipelines into a
minimal Python graph representation. Only the subset of DOT we care about is
extracted (node attributes, edge attributes, optional subgraph clusters).
"""

from __future__ import annotations

import pathlib
from dataclasses import dataclass, field
from typing import Optional

import pydot


@dataclass
class Node:
    name: str
    attrs: dict[str, str] = field(default_factory=dict)

    @property
    def shape(self) -> str:
        return self.attrs.get("shape", "ellipse")

    @property
    def prompt_ref(self) -> Optional[str]:
        ref = self.attrs.get("prompt")
        if not ref:
            return None
        return ref[1:] if ref.startswith("@") else ref


@dataclass
class Edge:
    src: str
    dst: str
    attrs: dict[str, str] = field(default_factory=dict)

    @property
    def condition(self) -> Optional[str]:
        return self.attrs.get("condition")

    @property
    def label(self) -> Optional[str]:
        return self.attrs.get("label")


@dataclass
class Graph:
    name: str
    goal: str
    nodes: dict[str, Node]
    edges: list[Edge]

    def outgoing(self, name: str) -> list[Edge]:
        return [e for e in self.edges if e.src == name]


def _strip(value: str) -> str:
    if value is None:
        return ""
    v = str(value).strip()
    if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
        v = v[1:-1]
    return v


def parse(path: pathlib.Path) -> Graph:
    """Load a .dot file and return a Graph."""
    raw = pydot.graph_from_dot_file(str(path))
    if not raw:
        raise ValueError(f"no graphs parsed from {path}")
    g = raw[0]
    name = _strip(g.get_name() or path.stem)

    goal = ""
    graph_attrs = g.get_attributes()
    if graph_attrs:
        goal = _strip(graph_attrs.get("goal", ""))

    nodes: dict[str, Node] = {}

    def collect_nodes(scope) -> None:
        for n in scope.get_nodes():
            nm = _strip(n.get_name())
            if nm in ("graph", "node", "edge"):
                continue
            attrs = {k: _strip(v) for k, v in (n.get_attributes() or {}).items()}
            nodes[nm] = Node(name=nm, attrs=attrs)
        for sub in scope.get_subgraphs():
            collect_nodes(sub)

    collect_nodes(g)

    edges: list[Edge] = []

    def collect_edges(scope) -> None:
        for e in scope.get_edges():
            src = _strip(e.get_source())
            dst = _strip(e.get_destination())
            attrs = {k: _strip(v) for k, v in (e.get_attributes() or {}).items()}
            edges.append(Edge(src=src, dst=dst, attrs=attrs))
        for sub in scope.get_subgraphs():
            collect_edges(sub)

    collect_edges(g)

    if "start" not in nodes or "exit" not in nodes:
        raise ValueError(
            f"{path}: pipeline must contain both 'start' and 'exit' nodes"
        )

    return Graph(name=name, goal=goal, nodes=nodes, edges=edges)
