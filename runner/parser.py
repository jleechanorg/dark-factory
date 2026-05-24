"""DOT → graph model.

Parses Graphviz DOT files produced for Attractor-pattern pipelines into a
minimal Python graph representation. Only the subset of DOT we care about is
extracted (node attributes, edge attributes, optional subgraph clusters).
"""

from __future__ import annotations

import pathlib
import re
from dataclasses import dataclass, field
from typing import Optional

import pydot

AttrValue = str | int | bool

_GRAPH_INT_ATTRS = {"default_max_retries"}
_NODE_INT_ATTRS = {"max_retries", "max_visits", "timeout", "join_quorum"}
_NODE_BOOL_ATTRS = {"allow_partial", "goal_gate", "validation", "parallel"}
_EDGE_INT_ATTRS = {"weight"}
_STYLE_RULE_RE = re.compile(
    r"(?P<selector>[^\n{]+)\{\s*(?P<body>[^}]*)\}",
    re.MULTILINE | re.DOTALL,
)


@dataclass
class Node:
    name: str
    attrs: dict[str, AttrValue] = field(default_factory=dict)

    @property
    def shape(self) -> str:
        return str(self.attrs.get("shape", "ellipse"))

    @property
    def prompt_ref(self) -> Optional[str]:
        ref = self.attrs.get("prompt")
        if not ref:
            return None
        ref = str(ref)
        return ref[1:] if ref.startswith("@") else ref


def is_start_node(node: Node) -> bool:
    """Identify a `start` node — by name or by DOT shape `Mdiamond`."""
    return node.name == "start" or node.shape == "Mdiamond"


def is_exit_node(node: Node) -> bool:
    """Identify an `exit` node — by name or by DOT shape `Msquare`."""
    return node.name == "exit" or node.shape == "Msquare"


@dataclass
class Edge:
    src: str
    dst: str
    attrs: dict[str, AttrValue] = field(default_factory=dict)

    @property
    def condition(self) -> Optional[str]:
        value = self.attrs.get("condition")
        return str(value) if value is not None else None

    @property
    def label(self) -> Optional[str]:
        value = self.attrs.get("label")
        return str(value) if value is not None else None


@dataclass
class Graph:
    name: str
    goal: str
    nodes: dict[str, Node]
    edges: list[Edge]
    attrs: dict[str, AttrValue] = field(default_factory=dict)

    def outgoing(self, name: str) -> list[Edge]:
        return [e for e in self.edges if e.src == name]


def is_start_node(node: Node) -> bool:
    return node.name == "start" or node.shape == "Mdiamond"


def is_exit_node(node: Node) -> bool:
    return node.name == "exit" or node.shape == "Msquare"


def _strip(value: str) -> str:
    if value is None:
        return ""
    v = str(value).strip()
    if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
        v = v[1:-1]
    return v


def _coerce_int(value: str, context: str) -> int:
    try:
        return int(value)
    except (TypeError, ValueError) as exc:
        raise ValueError(f"{context}: expected integer, got {value!r}") from exc


def _coerce_bool(value: str, context: str) -> bool:
    normalized = value.strip().lower()
    if normalized in {"true", "1", "yes"}:
        return True
    if normalized in {"false", "0", "no"}:
        return False
    raise ValueError(f"{context}: expected boolean, got {value!r}")


def _parse_model_style_rules(raw: str) -> list[tuple[str, dict[str, str]]]:
    """Parse a tiny CSS-like stylesheet into ordered selector → attrs rules.

    Supported syntax is intentionally tiny:

    ```
    * {backend: codex}
    node-name {timeout: 120}
    ```
    """
    rules: list[tuple[str, dict[str, str]]] = []
    for match in _STYLE_RULE_RE.finditer(raw):
        selector = match.group("selector").strip()
        body = match.group("body")
        if not selector or body is None:
            continue
        attrs: dict[str, str] = {}
        for decl in re.split(r";", body):
            decl = decl.strip()
            if not decl:
                continue
            if ":" in decl:
                key, value = decl.split(":", 1)
            elif "=" in decl:
                key, value = decl.split("=", 1)
            else:
                continue
            key = key.strip()
            value = value.strip().strip('"').strip("'")
            if key:
                attrs[key] = value
        if attrs:
            rules.append((selector, attrs))
    return rules


def _selector_matches(selector: str, node: Node) -> bool:
    if selector == "*":
        return True
    if selector.startswith("."):
        token = selector[1:]
        classes = str(node.attrs.get("class", "")).replace(",", " ").split()
        return token in classes
    return selector == node.name


def _apply_model_stylesheet(
    path: pathlib.Path,
    nodes: dict[str, Node],
    stylesheet: Optional[str],
) -> None:
    if not stylesheet:
        return
    stylesheet_path = pathlib.Path(stylesheet)
    if not stylesheet_path.is_absolute():
        stylesheet_path = (path.parent / stylesheet_path).resolve()
    if not stylesheet_path.exists():
        raise ValueError(f"{path}: model stylesheet missing {stylesheet_path!s}")
    raw = stylesheet_path.read_text(encoding="utf-8")
    rules = _parse_model_style_rules(raw)
    explicit_node_attrs = {name: set(node.attrs.keys()) for name, node in nodes.items()}
    for selector, attrs in rules:
        for node in nodes.values():
            if not _selector_matches(selector, node):
                continue
            for key, value in attrs.items():
                if key in explicit_node_attrs.get(node.name, set()):
                    continue
                node.attrs[key] = value


def _coerce_attrs(
    raw_attrs: dict[str, str],
    *,
    int_attrs: set[str],
    bool_attrs: set[str],
    context: str,
) -> dict[str, AttrValue]:
    attrs: dict[str, AttrValue] = {}
    for key, value in raw_attrs.items():
        if key in int_attrs:
            attrs[key] = _coerce_int(value, f"{context} attr {key!r}")
        elif key in bool_attrs:
            attrs[key] = _coerce_bool(value, f"{context} attr {key!r}")
        else:
            attrs[key] = value
    return attrs


def _is_builtin_stmt(name: str) -> bool:
    return name in {"", "\\n", "\n", "graph", "node", "edge"}


def _validate_condition(condition: str, context: str) -> None:
    if "!=" in condition:
        left, right = condition.split("!=", 1)
    elif "=" in condition:
        left, right = condition.split("=", 1)
    else:
        raise ValueError(f"{context}: malformed condition {condition!r}")
    if not left.strip() or not right.strip():
        raise ValueError(f"{context}: malformed condition {condition!r}")


def parse(path: pathlib.Path) -> Graph:
    """Load a .dot file and return a Graph."""
    raw = pydot.graph_from_dot_file(str(path))
    if not raw:
        raise ValueError(f"no graphs parsed from {path}")
    if len(raw) != 1:
        raise ValueError(f"{path}: expected exactly one directed digraph")
    g = raw[0]
    if g.get_type() != "digraph":
        raise ValueError(f"{path}: expected directed digraph, got {g.get_type()!r}")
    name = _strip(g.get_name() or path.stem)

    goal = ""
    graph_attrs = _coerce_attrs(
        {k: _strip(v) for k, v in (g.get_attributes() or {}).items()},
        int_attrs=_GRAPH_INT_ATTRS,
        bool_attrs=set(),
        context=f"{path}: graph",
    )
    if graph_attrs:
        goal = str(graph_attrs.get("goal", ""))

    nodes: dict[str, Node] = {}

    def collect_nodes(scope) -> None:
        nonlocal goal, graph_attrs
        for n in scope.get_nodes():
            nm = _strip(n.get_name())
            raw_attrs = {k: _strip(v) for k, v in (n.get_attributes() or {}).items()}
            if nm == "graph":
                graph_attrs.update(
                    _coerce_attrs(
                        raw_attrs,
                        int_attrs=_GRAPH_INT_ATTRS,
                        bool_attrs=set(),
                        context=f"{path}: graph",
                    )
                )
                if not goal:
                    goal = raw_attrs.get("goal", "")
                continue
            if _is_builtin_stmt(nm):
                continue
            attrs = _coerce_attrs(
                raw_attrs,
                int_attrs=_NODE_INT_ATTRS,
                bool_attrs=_NODE_BOOL_ATTRS,
                context=f"{path}: node {nm!r}",
            )
            nodes[nm] = Node(name=nm, attrs=attrs)
        for sub in scope.get_subgraphs():
            collect_nodes(sub)

    collect_nodes(g)

    edges: list[Edge] = []

    def collect_edges(scope) -> None:
        for e in scope.get_edges():
            src = _strip(e.get_source())
            dst = _strip(e.get_destination())
            attrs = _coerce_attrs(
                {k: _strip(v) for k, v in (e.get_attributes() or {}).items()},
                int_attrs=_EDGE_INT_ATTRS,
                bool_attrs=set(),
                context=f"{path}: edge {src!r}->{dst!r}",
            )
            edges.append(Edge(src=src, dst=dst, attrs=attrs))
        for sub in scope.get_subgraphs():
            collect_edges(sub)

    collect_edges(g)

    _apply_model_stylesheet(path, nodes, graph_attrs.get("model_stylesheet"))

    if not any(is_start_node(node) for node in nodes.values()) or not any(
        is_exit_node(node) for node in nodes.values()
    ):
        raise ValueError(
            f"{path}: pipeline must contain both 'start' and 'exit' nodes"
        )

    for edge in edges:
        if edge.src not in nodes:
            raise ValueError(f"{path}: edge references unknown source node {edge.src!r}")
        if edge.dst not in nodes:
            raise ValueError(f"{path}: edge references unknown destination node {edge.dst!r}")
        if edge.condition:
            _validate_condition(edge.condition, f"{path}: edge {edge.src!r}->{edge.dst!r}")
        if is_start_node(nodes[edge.dst]):
            raise ValueError(f"{path}: start node must not have incoming edges")
        if is_exit_node(nodes[edge.src]):
            raise ValueError(f"{path}: exit node must not have outgoing edges")

    start_name = _start_node_name(nodes)
    reachable = _reachable_from_start(start_name, edges)
    unreachable = sorted(set(nodes) - reachable)
    if unreachable:
        raise ValueError(f"{path}: unreachable nodes from start: {', '.join(unreachable)}")
    _validate_retry_targets(path, graph_attrs, nodes, edges)

    return Graph(name=name, goal=goal, nodes=nodes, edges=edges, attrs=graph_attrs)


def _start_node_name(nodes: dict[str, Node]) -> str:
    if "start" in nodes:
        return "start"
    for node in nodes.values():
        if is_start_node(node):
            return node.name
    return "start"


def _reachable_from_start(start_name: str, edges: list[Edge]) -> set[str]:
    seen = {start_name}
    frontier = [start_name]
    while frontier:
        current = frontier.pop()
        for edge in edges:
            if edge.src != current or edge.dst in seen:
                continue
            seen.add(edge.dst)
            frontier.append(edge.dst)
    return seen


def _validate_retry_targets(
    path: pathlib.Path,
    graph_attrs: dict[str, AttrValue],
    nodes: dict[str, Node],
    edges: list[Edge],
) -> None:
    graph_retry_target = str(graph_attrs.get("retry_target", "") or "")
    if graph_retry_target and graph_retry_target not in nodes:
        raise ValueError(f"{path}: graph retry_target references unknown node {graph_retry_target!r}")

    for node in nodes.values():
        node_retry_target = str(node.attrs.get("retry_target", "") or "")
        if node_retry_target and node_retry_target not in nodes:
            raise ValueError(
                f"{path}: node {node.name!r} retry_target references unknown node {node_retry_target!r}"
            )
        goal_gate = node.attrs.get("goal_gate", False)
        if not isinstance(goal_gate, bool) or not goal_gate:
            continue
        target = node_retry_target or graph_retry_target
        if target and not any(edge.src == node.name and edge.dst == target for edge in edges):
            raise ValueError(
                f"{path}: goal_gate node {node.name!r} retry_target {target!r} must be an outgoing edge"
            )
