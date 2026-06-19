"""Edge condition grammar + edge selection for the engine.

Splits out from `runner/engine.py` (see `docs/refactor/file-ownership-map.engine.md`).
Owns `_evaluate_expression` (recursive-descent), `_edge_matches`, `_lookup`,
`_is_decision_node`, the next-node pickers (`_pick_next`, `_pick_next_from_edges`),
`_choose_edge` + label/weight helpers, and the small attribute parsers
(`_attr_int`, `_allow_partial`).
"""

from __future__ import annotations

from typing import Optional

from .handlers import Context, Result
from .parser import Edge, Node, _tokenize_condition


def _is_decision_node(node: Optional[Node]) -> bool:
    if node is None:
        return False
    return node.shape == "hexagon" or node.attrs.get("type") == "conditional"


def _evaluate_expression(
    cond: str,
    last: Result,
    ctx: Optional[Context],
    is_decision: bool,
) -> bool:
    cond = cond.strip()
    if not cond:
        return True

    tokens = _tokenize_condition(cond)
    if tokens is None:
        # Tokenization failed — fall through to the malformed-condition
        # back-compat branch below (str.split on `!=` / `=`). This is the
        # de-facto contract for malformed conditions and must remain until
        # promoted to a first-class grammar extension.
        tokens = []

    idx = 0

    def peek() -> Optional[tuple[str, str]]:
        if idx < len(tokens):
            return tokens[idx]
        return None

    def consume(expected_kind: Optional[str] = None) -> tuple[str, str]:
        nonlocal idx
        if idx >= len(tokens):
            raise ValueError("Unexpected end of expression")
        tok = tokens[idx]
        if expected_kind and tok[0] != expected_kind:
            raise ValueError(f"Expected {expected_kind}, got {tok[0]}")
        idx += 1
        return tok

    def parse_expression() -> bool:
        left = parse_term()
        while True:
            t = peek()
            if t and t[0] == 'OR':
                consume()
                right = parse_term()
                left = left or right
            else:
                break
        return left

    def parse_term() -> bool:
        left = parse_factor()
        while True:
            t = peek()
            if t and t[0] == 'AND':
                consume()
                right = parse_factor()
                left = left and right
            else:
                break
        return left

    def parse_factor() -> bool:
        t = peek()
        if t and t[0] == 'NOT':
            consume()
            return not parse_factor()
        if t and t[0] == 'PAREN' and t[1] == '(':
            consume()
            val = parse_expression()
            consume('PAREN')
            return val

        key_tok = consume('WORD')
        k_val = key_tok[1]
        if k_val and not (k_val[0].isalpha() or k_val[0] == '_'):
            raise ValueError(f"Invalid key name: {k_val!r}")
        op_tok = peek()
        if not op_tok:
            outcome = _lookup("outcome", last, ctx, is_decision)
            return outcome == k_val

        op_kind = op_tok[0]
        if op_kind not in {'EQ', 'NEQ', 'CONTAINS', 'NOT_CONTAINS', 'IN', 'NOT_IN'}:
            outcome = _lookup("outcome", last, ctx, is_decision)
            return outcome == k_val

        consume()
        val_tok = consume()

        k = k_val
        v = val_tok[1]
        actual_val = _lookup(k, last, ctx, is_decision)

        if op_kind == 'EQ':
            return actual_val == v
        elif op_kind == 'NEQ':
            return actual_val != v
        elif op_kind == 'CONTAINS':
            return v in actual_val
        elif op_kind == 'NOT_CONTAINS':
            return v not in actual_val
        elif op_kind == 'IN':
            parts = [p.strip() for p in v.split(",")]
            return actual_val in parts
        elif op_kind == 'NOT_IN':
            parts = [p.strip() for p in v.split(",")]
            return actual_val not in parts

        return False

    try:
        res = parse_expression()
        if idx < len(tokens):
            return False
        return res
    except Exception:
        if "!=" in cond:
            k, v = cond.split("!=", 1)
            return _lookup(k.strip(), last, ctx, is_decision) != v.strip()
        if "=" in cond:
            k, v = cond.split("=", 1)
            return _lookup(k.strip(), last, ctx, is_decision) == v.strip()
        return False


def _edge_matches(
    edge: Edge,
    last: Result,
    ctx: Optional[Context] = None,
    current: Optional[Node] = None,
) -> bool:
    cond = edge.condition
    if not cond:
        return True
    is_decision = _is_decision_node(current)
    return _evaluate_expression(cond, last, ctx, is_decision)


def _lookup(
    key: str,
    last: Result,
    ctx: Optional[Context] = None,
    is_decision: bool = False,
) -> str:
    if is_decision and ctx is not None:
        if key in ctx.state:
            return str(ctx.state[key])
    if key == "outcome":
        return last.outcome
    return str(last.metadata.get(key, ""))


def _attr_int(node: Node, key: str, default: int) -> int:
    raw = node.attrs.get(key)
    if raw is None or raw == "":
        return default
    if isinstance(raw, bool):
        return default
    if isinstance(raw, int):
        return raw
    try:
        return int(raw)
    except (TypeError, ValueError):
        return default


def _allow_partial(node: Node) -> bool:
    allow_partial = node.attrs.get("allow_partial", False)
    if isinstance(allow_partial, bool):
        return allow_partial
    return str(allow_partial).strip().lower() in {"true", "1", "yes"}


def _pick_next(
    graph,
    current: Node,
    last: Result,
    ctx: Optional[Context] = None,
) -> Optional[Node]:
    candidates = graph.outgoing(current.name)
    # Prefer edges with a matching explicit condition; fall back to unconditional ones.
    matching = [e for e in candidates if e.condition and _edge_matches(e, last, ctx, current)]
    if matching:
        return graph.nodes.get(_choose_edge(matching, last).dst)
    unconditional = [e for e in candidates if not e.condition]
    if unconditional:
        return graph.nodes.get(_choose_edge(unconditional, last).dst)
    return None


def _pick_next_from_edges(
    graph,
    current: Node,
    edges: list[Edge],
    last: Result,
    ctx: Optional[Context] = None,
) -> Optional[Node]:
    # Keep behavior consistent with _pick_next() while allowing explicit edges.
    if not edges:
        return None
    matching = [edge for edge in edges if edge.condition and _edge_matches(edge, last, ctx, current)]
    if matching:
        selected = _choose_edge(matching, last)
        return graph.nodes.get(selected.dst)
    unconditional = [edge for edge in edges if not edge.condition]
    if unconditional:
        selected = _choose_edge(unconditional, last)
        return graph.nodes.get(selected.dst)
    return None


def _choose_edge(edges: list[Edge], last: Result) -> Edge:
    preferred_label = last.preferred_label or last.metadata.get("preferred_label", "")
    suggested_next_ids = list(last.suggested_next_ids)
    suggested_meta = last.metadata.get("suggested_next_ids") or last.metadata.get("suggested_next") or ""
    if suggested_meta:
        suggested_next_ids.extend(
            item.strip() for item in str(suggested_meta).split(",") if item.strip()
        )
    suggested_rank = {node_id: idx for idx, node_id in enumerate(suggested_next_ids)}

    def rank(edge: Edge) -> tuple[int, int, int, str, str]:
        return (
            0 if preferred_label and _normalize_label(edge.label or "") == _normalize_label(preferred_label) else 1,
            suggested_rank.get(edge.dst, len(suggested_rank)),
            -_edge_weight(edge),
            edge.dst,
            edge.label or "",
        )

    return sorted(edges, key=rank)[0]


def _normalize_label(label: str) -> str:
    text = label.strip().lower()
    if text.startswith("[") and "]" in text:
        text = text.split("]", 1)[1].strip()
    if text.startswith("(") and ")" in text:
        text = text.split(")", 1)[1].strip()
    if len(text) > 2 and text[1] in {")", ".", "-", ":"}:
        text = text[2:].strip()
    return " ".join(text.split())


def _edge_weight(edge: Edge) -> int:
    raw = edge.attrs.get("weight", 0)
    if isinstance(raw, bool):
        return 0
    if isinstance(raw, int):
        return raw
    try:
        return int(raw)
    except (TypeError, ValueError):
        return 0
