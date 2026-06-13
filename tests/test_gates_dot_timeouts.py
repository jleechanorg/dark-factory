"""Regression guard: every node in pipelines/factory/gates.dot must declare a timeout.

Pinned contract: a gate that runs without a `timeout=` attribute can hang
indefinitely (the `claude --print` / `codex exec` / etc. subprocess has
no upper bound), which has been the most common "stuck run" failure
mode in dark-factory observability (bead jleechan-sp6 P2 "monitor
declare DONE only on exit node recorded"). The sibling pipeline
`pipelines/factory/pr_gates.dot` already declares `timeout=600` on
every node — `gates.dot` must do the same so a stray gate_hang
silently turns into a clean `exhausted` outcome and the run can
recover.

File-disjoint: this test only reads `pipelines/factory/gates.dot`
through the existing parser. Does not touch any other runner or test
file.
"""

from __future__ import annotations

import pathlib
import sys

ROOT = pathlib.Path(__file__).parent.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(pathlib.Path(__file__).parent))

from conftest import _pipeline  # noqa: E402

from runner.parser import parse  # noqa: E402


# Every node type that can run a subprocess in dark-factory. A node
# declared with one of these types MUST have a `timeout` attribute, or
# the run is at risk of hanging forever if the subprocess stalls.
_SUBPROCESS_NODE_TYPES = frozenset(
    {
        "codergen",
        "tool",
        "holdout_eval",
        "gate_es",
        "gate_er",
        "gate_code_standards",
        "human_gate",
        "agy",
        "ao",
    }
)


def test_gates_dot_every_node_declares_a_timeout() -> None:
    """Pinned regression: a `gates.dot` run must never hang forever.

    Iterates every node in `pipelines/factory/gates.dot` and asserts
    that any node whose type can run a subprocess has a `timeout`
    attribute. The pin is the same `timeout=600` value used by the
    sibling `pr_gates.dot` so a future drift between the two pipelines
    shows up in CI before a real hang.
    """
    g = parse(_pipeline("gates.dot"))
    missing: list[str] = []
    for name, node in g.nodes.items():
        # Built-in start/exit markers don't run subprocesses.
        if name in {"start", "exit"}:
            continue
        node_type = node.attrs.get("type", "")
        if node_type in _SUBPROCESS_NODE_TYPES:
            if "timeout" not in node.attrs:
                missing.append(f"{name} (type={node_type})")
    assert not missing, (
        "gates.dot nodes must declare a timeout= to prevent indefinite "
        f"hangs. Missing: {missing}. Use the same timeout=600 as the "
        "sibling pr_gates.dot."
    )


def test_gates_dot_timeout_matches_sibling_pr_gates_dot() -> None:
    """The `gates.dot` timeouts must agree with the sibling `pr_gates.dot`.

    Two pipelines that compose the same 3-gate chain should not
    silently diverge on the per-node timeout — if one says 600 and the
    other says 60, a future maintainer will be debugging "why does
    the same gate hang in one pipeline and not the other" for an hour.
    """
    g_gates = parse(_pipeline("gates.dot"))
    g_pr = parse(_pipeline("pr_gates.dot"))

    common = {"holdout", "gate_es", "gate_er", "gate_cs"}
    for name in common:
        assert name in g_gates.nodes, f"gates.dot missing node {name}"
        assert name in g_pr.nodes, f"pr_gates.dot missing node {name}"
        g_to = g_gates.nodes[name].attrs.get("timeout")
        pr_to = g_pr.nodes[name].attrs.get("timeout")
        assert g_to is not None, f"gates.dot {name} missing timeout"
        assert pr_to is not None, f"pr_gates.dot {name} missing timeout"
        assert g_to == pr_to, (
            f"gates.dot and pr_gates.dot disagree on {name} timeout: "
            f"gates.dot={g_to}, pr_gates.dot={pr_to}"
        )
