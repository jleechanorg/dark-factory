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
_NODE_INT_ATTRS = {"max_retries", "max_visits", "timeout", "join_quorum", "k"}
_NODE_BOOL_ATTRS = {"allow_partial", "goal_gate", "validation", "parallel"}
_EDGE_INT_ATTRS = {"weight"}
_STYLE_RULE_RE = re.compile(
    r"(?P<selector>[^\n{]+)\{\s*(?P<body>[^}]*)\}",
    re.MULTILINE | re.DOTALL,
)
_VALIDATION_TIMEOUT_MIN_SECONDS = 60
_VALIDATION_TYPES = {"holdout_eval", "gate_es", "gate_er", "gate_code_standards"}


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
    condition = (condition or "").strip()
    if not condition:
        raise ValueError(f"{context}: malformed condition {condition!r}")

    token_specification = [
        ('PAREN', r'[()]'),
        ('AND', r'&&|\band\b'),
        ('OR', r'\|\||\bor\b'),
        ('NOT_CONTAINS', r'not\s+contains\b'),
        ('CONTAINS', r'contains\b'),
        ('NOT_IN', r'not\s+in\b'),
        ('IN', r'\bin\b'),
        ('NEQ', r'!='),
        ('EQ', r'==|='),
        ('NOT', r'!|not\b(?=\s|\(|\)|$)'),
        ('STRING', r'"[^"]*"|\'[^\']*\''),
        ('WORD', r'[a-zA-Z_0-9\-\.\*]+'),
        ('SPACE', r'\s+'),
    ]
    pattern = re.compile('|'.join(f'(?P<{kind}>{expr})' for kind, expr in token_specification))
    tokens: list[tuple[str, str]] = []

    pos = 0
    for match in pattern.finditer(condition):
        if match.start() != pos:
            raise ValueError(f"{context}: malformed condition {condition!r}")
        pos = match.end()
        kind = match.lastgroup
        value = match.group()
        if kind == 'SPACE':
            continue
        if kind == 'STRING':
            value = value[1:-1]
            tokens.append((kind, value))
        else:
            tokens.append((kind, value))

    if not tokens:
        raise ValueError(f"{context}: malformed condition {condition!r}")
    if pos != len(condition):
        raise ValueError(f"{context}: malformed condition {condition!r}")

    index = 0

    def peek() -> tuple[str, str] | None:
        nonlocal index
        if index < len(tokens):
            return tokens[index]
        return None

    def consume(expected_kind: str | None = None) -> tuple[str, str]:
        nonlocal index
        if index >= len(tokens):
            raise ValueError(f"{context}: malformed condition {condition!r}")
        token = tokens[index]
        index += 1
        if expected_kind is not None and token[0] != expected_kind:
            raise ValueError(f"{context}: malformed condition {condition!r}")
        return token

    def parse_factor() -> None:
        token = peek()
        if token is None:
            raise ValueError(f"{context}: malformed condition {condition!r}")
        if token[0] == 'NOT':
            consume()
            parse_factor()
            return
        if token[0] in {'NOT_IN', 'NOT_CONTAINS', 'CONTAINS', 'IN', 'NEQ', 'EQ', 'AND', 'OR'}:
            raise ValueError(f"{context}: malformed condition {condition!r}")

        if token[0] == 'PAREN' and token[1] == '(':
            consume()
            parse_expression()
            closing = peek()
            if closing is None or closing[0] != 'PAREN' or closing[1] != ')':
                raise ValueError(f"{context}: malformed condition {condition!r}")
            consume()
            return

        consume('WORD')
        token = peek()
        if token is None:
            return
        if token[0] in {'PAREN', 'AND', 'OR'}:
            return
        if token[0] in {'NEQ', 'EQ', 'CONTAINS', 'NOT_CONTAINS', 'IN', 'NOT_IN'}:
            consume()
            if peek() is None:
                raise ValueError(f"{context}: malformed condition {condition!r}")
            if peek()[0] == 'PAREN':
                consume('PAREN')
                parse_expression()
                close = peek()
                if close is None or close[0] != 'PAREN' or close[1] != ')':
                    raise ValueError(f"{context}: malformed condition {condition!r}")
                consume()
                return
            consume()
            return
        if token[0] == 'NOT':
            consume()
            parse_factor()
            return
        if token[0] != 'SPACE':
            raise ValueError(f"{context}: malformed condition {condition!r}")

    def parse_expression() -> None:
        parse_term()
        while True:
            token = peek()
            if token is None:
                return
            if token[0] != 'OR':
                return
            consume()
            parse_term()

    def parse_term() -> None:
        parse_factor()
        while True:
            token = peek()
            if token is None or token[0] != 'AND':
                return
            consume()
            parse_factor()

    parse_expression()
    if peek() is not None:
        raise ValueError(f"{context}: malformed condition {condition!r}")


def _resolve_includes(
    path: pathlib.Path,
    nodes: dict[str, Node],
    edges: list[Edge],
    graph_attrs: dict[str, AttrValue],
    require_start_exit: bool,
    visited: tuple[pathlib.Path, ...] = (),
) -> None:
    """Process graph-attr ``include="@path[@;path...]"`` on the parent.

    For each path (one or more, semicolon-separated), parse the included
    .dot file as a *library* and merge its nodes and edges into the
    parent's ``nodes`` and ``edges`` collections. The included file must
    not contain ``start`` or ``exit`` nodes (libraries don't run directly);
    node names must not collide with the parent's already-declared nodes
    (lanes must not redefine library nodes).

    Path resolution order (mirrors the ``prompt="@..."`` convention so
    lanes in subdirectories can write repo-root-relative paths):

      1. Absolute path.
      2. Relative to the parent .dot file's directory.
      3. Relative to the current working directory.

    The leading ``@`` is optional and stripped.

    Cycle detection: ``visited`` is the chain of resolved include paths
    leading to the current call. If a path tries to re-include any
    ancestor in that chain, we raise a parse error rather than recurse
    forever. The first call from ``_parse`` passes an empty tuple; every
    recursive call appends the resolved ``include_path`` before recursing.
    """
    raw_attr = str(graph_attrs.get("include") or "")
    if not raw_attr:
        return
    parent_dir = path.resolve().parent
    cwd = pathlib.Path.cwd()
    for raw in raw_attr.split(";"):
        ref = raw.strip()
        if not ref:
            continue
        if ref.startswith("@"):
            ref = ref[1:]
        if not ref:
            continue
        include_path: pathlib.Path | None = None
        tried: list[pathlib.Path] = []
        for base in (parent_dir, cwd):
            candidate = (base / ref).resolve()
            tried.append(candidate)
            if candidate.exists():
                include_path = candidate
                break
        if include_path is None:
            raise ValueError(
                f"{path}: include not found: {ref!r} "
                f"(tried parent-relative {tried[0]} and cwd-relative {tried[1]})"
            )
        if include_path in visited:
            chain = " -> ".join(str(p) for p in (*visited, include_path))
            raise ValueError(
                f"{path}: include cycle detected: {chain}"
            )
        sub = _parse(include_path, require_start_exit=False, visited=(*visited, include_path))
        # Reject any start/exit in the included graph — libraries must
        # be runnable-only-when-wired-into-a-lane, not standalone.
        for nm, node in sub.nodes.items():
            if is_start_node(node) or is_exit_node(node):
                raise ValueError(
                    f"{include_path}: included file declares {nm!r} node; "
                    f"library .dot files must not declare start or exit"
                )
            if nm in nodes:
                raise ValueError(
                    f"{path}: include {ref!r} declares node {nm!r} which "
                    f"collides with a parent node — lanes must not redefine "
                    f"library nodes"
                )
            nodes[nm] = node
        # Edges from the included graph are appended. They may reference
        # either library nodes (already merged) or parent-declared nodes
        # (which are wired in by the parent graph's own edges).
        for e in sub.edges:
            if e.src in nodes and e.dst in nodes:
                edges.append(e)
            else:
                # Edges that reference a node the parent hasn't declared
                # and the include didn't declare are silently dropped —
                # they will be re-declared by the parent as wiring edges.
                # This lets a library expose `in -> out` point nodes and
                # the parent wire them up.
                pass


def parse(path: pathlib.Path, require_start_exit: bool = True) -> Graph:
    """Load a .dot file and return a Graph.

    By default pipelines must contain both ``start`` and ``exit`` nodes.
    Pass ``require_start_exit=False`` to load a graph *library* (used by
    ``include="@path"`` and by tests that want to inspect a library directly).
    """
    return _parse(path, require_start_exit=require_start_exit)


def _parse(
    path: pathlib.Path,
    require_start_exit: bool,
    visited: tuple[pathlib.Path, ...] = (),
) -> Graph:
    """Internal: load a .dot file and return a Graph.

    When ``require_start_exit`` is False (used for graph-library includes),
    the start/exit presence check is skipped — the included file is a
    library, not a runnable pipeline, so it does not need them.

    ``visited`` is the chain of resolved include paths leading here, used
    by ``_resolve_includes`` to detect cycles (see its docstring).
    """
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

    # Resolve graph-attr `include="@path[@;path...]"` BEFORE start/exit
    # validation. Included files are libraries; their content is merged into
    # the parent's node and edge sets. The parent's start/exit requirement
    # still applies; the included graph must not declare its own.
    if graph_attrs.get("include"):
        _resolve_includes(
            path, nodes, edges, graph_attrs, require_start_exit, visited
        )

    _apply_model_stylesheet(path, nodes, graph_attrs.get("model_stylesheet"))

    if require_start_exit and (
        not any(is_start_node(node) for node in nodes.values())
        or not any(is_exit_node(node) for node in nodes.values())
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
    if require_start_exit:
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


def _node_has_validation(node: Node) -> bool:
    if node.attrs.get("type") in _VALIDATION_TYPES:
        return True
    raw = node.attrs.get("validation", False)
    if isinstance(raw, bool):
        return raw
    return str(raw).strip().lower() in {"true", "1", "yes"}


def validate_pipeline(path: pathlib.Path) -> tuple[Optional[Graph], list[dict[str, str]]]:
    """Validate a pipeline and emit machine-readable diagnostics.

    Returns `(graph, diagnostics)`. When parsing/validation fails, `graph` is
    `None` and diagnostics contain at least one error entry.
    """
    pipeline_path = pathlib.Path(path)
    diagnostics: list[dict[str, str]] = []

    if not pipeline_path.exists():
        diagnostics.append(
            {
                "code": "DF_PIPELINE_NOT_FOUND",
                "severity": "error",
                "node": "<pipeline>",
                "message": f"pipeline file does not exist: {pipeline_path}",
            }
        )
        return None, diagnostics

    try:
        graph = parse(pipeline_path)
    except Exception as exc:
        diagnostics.append(
            {
                "code": "DF_PIPELINE_PARSE_ERROR",
                "severity": "error",
                "node": "<pipeline>",
                "message": f"failed to parse pipeline: {type(exc).__name__}: {exc}",
            }
        )
        return None, diagnostics

    for node in graph.nodes.values():
        if node.attrs.get("type") == "codergen":
            prompt_ref = node.prompt_ref
            if not prompt_ref:
                diagnostics.append(
                    {
                        "code": "DF_MISSING_PROMPT",
                        "severity": "error",
                        "node": node.name,
                        "message": "codergen node missing prompt path",
                    }
                )
            else:
                prompt_path = pathlib.Path(prompt_ref)
                if not prompt_path.is_absolute():
                    prompt_path = (pipeline_path.parent / prompt_path).resolve()
                if not prompt_path.exists():
                    diagnostics.append(
                        {
                            "code": "DF_MISSING_PROMPT",
                            "severity": "error",
                            "node": node.name,
                            "message": f"codergen prompt file not found: {prompt_path}",
                        }
                    )

        if _node_has_validation(node):
            timeout = node.attrs.get("timeout")
            if timeout is None:
                diagnostics.append(
                    {
                        "code": "DF_MISSING_VALIDATION_TIMEOUT",
                        "severity": "error",
                        "node": node.name,
                        "message": "validation node missing timeout",
                    }
                )
            elif isinstance(timeout, int) and timeout < _VALIDATION_TIMEOUT_MIN_SECONDS:
                diagnostics.append(
                    {
                        "code": "DF_VALIDATION_TIMEOUT_TOO_LOW",
                        "severity": "error",
                        "node": node.name,
                        "message": (
                            f"validation timeout too low: {timeout}s < "
                            f"minimum {_VALIDATION_TIMEOUT_MIN_SECONDS}s"
                        ),
                    }
                )

    return graph, diagnostics
