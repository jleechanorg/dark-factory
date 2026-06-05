"""Graph-IR generator for workflow_graphgen.

The spec defines the generator as a Claude Workflow running on Opus that reads
the goal and returns a graph description. For the benchmark, generation must be
**identical across both modes** ("generated once per goal and shared by both
modes" — fairness control), so the default generator here is deterministic: it
emits a linear dynamic middle for the goal. A live Opus Workflow generator can be
plugged in by passing its returned dict to ``GraphIR.from_dict`` — both modes
then share that single IR.

A linear middle (no internal fix loop) is used for the smoke so that Mode A's
runner walk and Mode A+B's harness-driven execution of the middle are trivially
identical. The vocabulary still permits richer shapes; the smoke just does not
exercise them.
"""

from __future__ import annotations

from .catalog import load_catalog
from .graph_ir import GUARANTEED_NODES, EdgeIR, GraphIR, NodeIR

DEFAULT_CODER_BACKEND = "claude"
DEFAULT_CODER_MODEL = "claude-sonnet-4-6"


def deterministic_generator(
    goal: str,
    *,
    backend: str = DEFAULT_CODER_BACKEND,
    model_name: str | None = DEFAULT_CODER_MODEL,
) -> GraphIR:
    """Return a GraphIR with a linear plan -> implement middle for ``goal``.

    The coder backend/model are pinned on every middle node so both modes run the
    identical coder. Reviewer tail is fixed by the renderer.
    """
    catalog = load_catalog()["prompts"]
    middle_types = ["plan", "implement"]
    nodes = [
        NodeIR(
            name=t,
            type=t,
            prompt=catalog[t],
            backend=backend,
            model_name=model_name,
        )
        for t in middle_types
    ]
    edges = [EdgeIR(src=a, dst=b) for a, b in zip(middle_types, middle_types[1:])]
    return GraphIR(
        nodes=nodes,
        edges=edges,
        guaranteed=list(GUARANTEED_NODES),
        rationale=(
            "Linear plan->implement middle keeps Mode A (runner walk) and Mode A+B "
            "(harness-driven) execution byte-identical; fixed terminal reviewer tail "
            "is injected by the renderer."
        ),
    )
