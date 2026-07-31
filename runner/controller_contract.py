"""Deterministic v1 controller-contract inspector.

Design:
docs/superpowers/specs/2026-07-30-slim-feature-repo-factory-design.md

Scope:
- ``inspect(workdir, profile)`` validates the target repo's
  ``dark-factory/factory.toml`` + ``dark-factory/pipelines/factory_<profile>.dot``
  against the v1 contract and returns a deterministic JSON string.
- It NEVER calls a model, executes an action, mutates the target, issues
  trusted receipts, or claims ownership / fencing / checkpoint capability.
  All output carries ``execution_enabled=false``.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import re
import tomllib
from collections.abc import Mapping
from typing import Any

from .parser import Node, parse

SUPPORTED_PROFILES = ("slim", "feature")
SUPPORTED_SCHEMA_VERSION = 1
SUPPORTED_CONTROLLER_API_VERSION = 1

ALLOWED_RECEIPT_CLASSES = frozenset(
    {"test", "behavioral_proof", "evidence", "independent_review"}
)
ALLOWED_ACTION_KINDS = frozenset({"tool", "holdout_eval", "slash"})
ALLOWED_ACTION_KEYS = frozenset(
    {"kind", "argv", "cwd", "timeout_seconds", "receipt_class", "name"}
)
ALLOWED_CONTROLLER_KEYS = frozenset(
    {"infra_retry_attempts", "suspension_max_age_seconds", "artifact_retention_days"}
)
ALLOWED_PROFILE_KEYS = {
    "slim": frozenset({"verify", "repair_passes"}),
    "feature": frozenset({"prove", "review", "repair_passes"}),
}
ALLOWED_TOP_LEVEL_KEYS = frozenset(
    {"schema_version", "controller_api_version", "actions", "controller", "profiles"}
)

DISALLOWED_TOKEN_RE = re.compile(r"[$`|;&<>\n\r\\]|/bin/sh|/bin/bash|\\$\\{")

SLIM_NODES = {
    "start": {"shape": "Mdiamond"},
    "work": {"type": "codergen", "max_visits": 2, "max_retries": 0},
    "verify": {"type": "factory_verify", "max_retries": 0},
    "exit": {"shape": "Msquare"},
}
SLIM_EDGES = {
    ("start", "work"): None,
    ("work", "verify"): None,
    ("verify", "exit"): "outcome=success",
    ("verify", "work"): "outcome=failure",
}

FEATURE_NODES = {
    "start": {"shape": "Mdiamond"},
    "spec": {"type": "codergen", "max_visits": 1, "max_retries": 0},
    "build": {"type": "codergen", "max_visits": 2, "max_retries": 0},
    "prove": {"type": "factory_prove", "max_retries": 0},
    "independent_review": {
        "type": "factory_independent_review",
        "prefer_adversarial": "true",
        "max_retries": 0,
    },
    "exit": {"shape": "Msquare"},
}
FEATURE_EDGES = {
    ("start", "spec"): None,
    ("spec", "build"): None,
    ("build", "prove"): None,
    ("prove", "independent_review"): None,
    ("independent_review", "exit"): "outcome=success",
    ("prove", "build"): "outcome=failure",
    ("independent_review", "build"): "outcome=failure",
}

TOPOLOGY = {
    "slim": {"nodes": SLIM_NODES, "edges": SLIM_EDGES, "node_count": 4},
    "feature": {"nodes": FEATURE_NODES, "edges": FEATURE_EDGES, "node_count": 6},
}


class ContractError(ValueError):
    """v1 contract violation — caller's CLI translates to fail-closed JSON."""


def _unsafe(value: str) -> bool:
    return bool(DISALLOWED_TOKEN_RE.search(value))


def _check_safe_str(value: Any, context: str) -> str:
    if not isinstance(value, str):
        raise ContractError(f"{context}: expected string, got {type(value).__name__}")
    if _unsafe(value):
        raise ContractError(f"unsafe token in {context}: {value!r}")
    return value


def _require_path_within(workdir: pathlib.Path, value: str, context: str) -> pathlib.Path:
    if not value:
        raise ContractError(f"{context}: path must be non-empty")
    if _unsafe(value):
        raise ContractError(f"unsafe path token in {context}: {value!r}")
    candidate = (workdir / value).resolve()
    try:
        candidate.relative_to(workdir)
    except ValueError as exc:
        raise ContractError(f"{context}: escapes workdir: {value!r}") from exc
    return candidate


def _check_node(node: Node, name: str, spec: Mapping[str, Any]) -> None:
    if "shape" in spec and node.shape != spec["shape"]:
        raise ContractError(
            f"node {name!r}: shape must be {spec['shape']!r}, got {node.shape!r}"
        )
    if "type" in spec:
        actual = str(node.attrs.get("type") or "") or None
        if actual != spec["type"]:
            raise ContractError(
                f"node {name!r}: type must be {spec['type']!r}, got {actual!r}"
            )
    for key in ("max_visits", "max_retries"):
        if key in spec:
            actual = int(str(node.attrs.get(key) or 0))
            if actual != spec[key]:
                raise ContractError(
                    f"node {name!r}: {key} must be {spec[key]}, got {actual}"
                )
    if "prefer_adversarial" in spec:
        actual = str(node.attrs.get("prefer_adversarial") or "")
        if actual != spec["prefer_adversarial"]:
            raise ContractError(
                f"node {name!r}: prefer_adversarial must be "
                f"{spec['prefer_adversarial']!r}, got {actual!r}"
            )


def _validate_graph(graph_path: pathlib.Path, profile: str) -> list[dict[str, Any]]:
    graph = parse(graph_path)
    spec = TOPOLOGY[profile]
    if sorted(graph.nodes) != sorted(spec["nodes"]):
        raise ContractError(
            f"{graph_path.name}: nodes must be {sorted(spec['nodes'])}, "
            f"got {sorted(graph.nodes)}"
        )
    for name, attrs in spec["nodes"].items():
        _check_node(graph.nodes[name], name, attrs)
    actual_edges = {(e.src, e.dst): e.condition for e in graph.edges}
    if actual_edges != spec["edges"]:
        raise ContractError(
            f"{graph_path.name}: edges must exactly match approved topology"
        )
    return [
        {"name": name, "attrs": dict(node.attrs)} for name, node in sorted(graph.nodes.items())
    ]


def _validate_actions(
    actions: Mapping[str, Any], base: pathlib.Path
) -> dict[str, dict[str, Any]]:
    if not isinstance(actions, Mapping):
        raise ContractError("[actions] must be a table")
    validated: dict[str, dict[str, Any]] = {}
    for action_id, raw in actions.items():
        if not isinstance(raw, Mapping):
            raise ContractError(f"action {action_id!r} must be a table")
        unknown = sorted(set(raw) - ALLOWED_ACTION_KEYS)
        if unknown:
            raise ContractError(f"action {action_id!r} has unknown keys: {unknown}")
        kind = _check_safe_str(raw.get("kind"), f"actions.{action_id}.kind")
        if kind not in ALLOWED_ACTION_KINDS:
            raise ContractError(
                f"actions.{action_id}.kind={kind!r} must be in {sorted(ALLOWED_ACTION_KINDS)}"
            )
        receipt_class = _check_safe_str(
            raw.get("receipt_class"), f"actions.{action_id}.receipt_class"
        )
        if receipt_class not in ALLOWED_RECEIPT_CLASSES:
            raise ContractError(
                f"actions.{action_id}.receipt_class={receipt_class!r} "
                f"must be in {sorted(ALLOWED_RECEIPT_CLASSES)}"
            )
        timeout = raw.get("timeout_seconds")
        if not isinstance(timeout, int) or timeout <= 0:
            raise ContractError(
                f"actions.{action_id}.timeout_seconds must be a positive integer"
            )

        if kind == "tool":
            argv = raw.get("argv")
            if not isinstance(argv, list) or not argv:
                raise ContractError(f"actions.{action_id}: tool actions need non-empty argv")
            cleaned_argv = [_check_safe_str(t, f"actions.{action_id}.argv") for t in argv]
            cwd = _check_safe_str(raw.get("cwd"), f"actions.{action_id}.cwd")
            _require_path_within(base, cwd, f"actions.{action_id}.cwd")
            validated[action_id] = {
                "kind": "tool",
                "argv": cleaned_argv,
                "cwd": cwd,
                "receipt_class": receipt_class,
            }
        elif kind == "slash":
            name = _check_safe_str(raw.get("name"), f"actions.{action_id}.name")
            validated[action_id] = {
                "kind": "slash",
                "name": name,
                "receipt_class": receipt_class,
            }
        else:  # holdout_eval
            if any(k in raw for k in ("argv", "cwd", "name")):
                raise ContractError(
                    f"holdout_eval action {action_id!r} must not declare target-controlled "
                    f"argv/cwd/name; the holdout path is sealed"
                )
            validated[action_id] = {
                "kind": "holdout_eval",
                "receipt_class": receipt_class,
            }
    return validated


def _validate_controller(controller: Any) -> None:
    if not isinstance(controller, Mapping):
        raise ContractError("[controller] must be a table")
    unknown = sorted(set(controller) - ALLOWED_CONTROLLER_KEYS)
    if unknown:
        raise ContractError(f"[controller] has unknown keys: {unknown}")


def _expand_profile_actions(
    profile: str, profile_table: Mapping[str, Any], action_ids: set[str]
) -> list[dict[str, Any]] | dict[str, list[dict[str, Any]]]:
    """Return either an ordered list (slim) or stage-keyed lists (feature)."""
    allowed = ALLOWED_PROFILE_KEYS[profile]
    unknown = sorted(set(profile_table) - allowed)
    if unknown:
        raise ContractError(f"[profiles.{profile}] has unknown keys: {unknown}")
    if profile_table.get("repair_passes") != 1:
        raise ContractError(
            f"[profiles.{profile}].repair_passes must equal 1 in v1; "
            f"got {profile_table.get('repair_passes')!r}"
        )

    def _flatten(stage: str) -> list[dict[str, str]]:
        actions = profile_table.get(stage)
        if not isinstance(actions, list) or not actions:
            raise ContractError(f"[profiles.{profile}].{stage} must be a non-empty list")
        return [
            {"action_id": _check_safe_str(a, f"profiles.{profile}.{stage}")}
            for a in actions
            if a in action_ids
            or _raise_unknown_action(profile, stage, a, action_ids)
        ]

    if profile == "slim":
        return _flatten("verify")
    return {
        "prove": _flatten("prove"),
        "review": _flatten("review"),
    }


def _raise_unknown_action(profile: str, stage: str, action_id: Any, known: set[str]) -> bool:
    if not isinstance(action_id, str):
        raise ContractError(f"[profiles.{profile}].{stage} entries must be strings")
    if action_id not in known:
        raise ContractError(
            f"[profiles.{profile}].{stage} references unknown action {action_id!r}"
        )
    return True


def _payload_for_plan(plan: Any, actions: Mapping[str, dict[str, Any]]) -> Any:
    if isinstance(plan, list):
        return [
            {"action_id": entry["action_id"], "receipt_class": actions[entry["action_id"]]["receipt_class"]}
            for entry in plan
        ]
    return {
        stage: [
            {"action_id": e["action_id"], "receipt_class": actions[e["action_id"]]["receipt_class"]}
            for e in entries
        ]
        for stage, entries in plan.items()
    }


def inspect(workdir: str | pathlib.Path, profile: str) -> str:
    """Validate the v1 controller contract. Returns JSON, raises :class:`ContractError`."""
    if profile not in SUPPORTED_PROFILES:
        raise ContractError(
            f"profile must be one of {list(SUPPORTED_PROFILES)}; got {profile!r}"
        )

    base = pathlib.Path(workdir).expanduser().resolve()
    if not base.is_dir():
        raise ContractError(f"workdir does not exist or is not a directory: {base}")

    manifest_path = base / "dark-factory" / "factory.toml"
    pipeline_path = base / "dark-factory" / "pipelines" / f"factory_{profile}.dot"
    if not manifest_path.is_file():
        raise ContractError(f"manifest not found: {manifest_path}")
    if not pipeline_path.is_file():
        raise ContractError(f"pipeline not found: {pipeline_path}")

    raw = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    unknown_top = sorted(set(raw) - ALLOWED_TOP_LEVEL_KEYS)
    if unknown_top:
        raise ContractError(f"unknown top-level manifest keys: {unknown_top}")
    if raw.get("schema_version") != SUPPORTED_SCHEMA_VERSION:
        raise ContractError(
            f"unsupported schema_version={raw.get('schema_version')!r}; "
            f"expected {SUPPORTED_SCHEMA_VERSION}"
        )
    if raw.get("controller_api_version") != SUPPORTED_CONTROLLER_API_VERSION:
        raise ContractError(
            f"unsupported controller_api_version={raw.get('controller_api_version')!r}; "
            f"expected {SUPPORTED_CONTROLLER_API_VERSION}"
        )

    actions = _validate_actions(raw.get("actions", {}), base)
    _validate_controller(raw.get("controller", {}))
    profiles = raw.get("profiles", {})
    if profile not in profiles:
        raise ContractError(f"profile {profile!r} not declared in [profiles]")

    plan = _expand_profile_actions(profile, profiles[profile], set(actions.keys()))
    # Validate the plan only references known actions.
    if isinstance(plan, list):
        for entry in plan:
            entry["action_id"]  # already string-validated above
    else:
        for stage_entries in plan.values():
            for entry in stage_entries:
                entry["action_id"]  # already string-validated

    topology_nodes = _validate_graph(pipeline_path, profile)

    payload = {
        "manifest_path": str(manifest_path),
        "pipeline_path": str(pipeline_path),
        "manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
        "pipeline_sha256": hashlib.sha256(pipeline_path.read_bytes()).hexdigest(),
        "profile": profile,
        "schema_version": SUPPORTED_SCHEMA_VERSION,
        "controller_api_version": SUPPORTED_CONTROLLER_API_VERSION,
        "repair_passes": 1,
        "execution_enabled": False,
        "status": "pass",
        "action_plan": _payload_for_plan(plan, actions),
        "topology": {
            "nodes": topology_nodes,
            "expected_node_count": TOPOLOGY[profile]["node_count"],
        },
    }
    return json.dumps(payload, sort_keys=True, indent=2)
