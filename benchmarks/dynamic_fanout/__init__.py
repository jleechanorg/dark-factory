"""dynamic_fanout — a *deterministic* A-vs-A+B separation benchmark.

Why this module exists
----------------------
The sibling ``workflow_graphgen`` benchmark measured Mode A (the Python runner
walks a static middle graph) vs Mode A+B (the harness fans the same middle out
node-by-node) and found **no separation on any axis at n=10** (see
``benchmarks/workflow_graphgen/RESULTS.md``). That null is honest — but a null
from a measuring instrument is only meaningful if the instrument can detect a
real effect when one exists. Otherwise the null could just mean "blind ruler."

``dynamic_fanout`` is that calibration: three scenarios engineered so the
dispatch path is *load-bearing*, graded on **real on-disk artifacts** by the
**same** ``benchmarks.workflow_graphgen.scoring.aggregate`` machinery (same
range-non-overlap rule, same ``MIN_N_FOR_WINNER`` guard). If that aggregator now
credits a winner, it proves:

  1. the instrument detects a mechanism difference when one is present, so
  2. the workflow_graphgen n=10 null was a **true negative**, not a blind ruler.

The capability gaps under test (from the brainstorm framework)
--------------------------------------------------------------
* **G3 — runtime-determined graph shape** (``validate_endpoints``):
  a feature exposes K endpoints (K only known at runtime). Mode A's graph is
  authored once with a *fixed* node count F, so it covers ``min(F, K)/K`` — it
  structurally under-covers whenever ``K > F`` and over-spends whenever
  ``K < F``. Mode A+B discovers K and fans out to exactly K, covering 1.0. A
  static ``.dot`` cannot make its own node count depend on a runtime count;
  this is the genuine paradigm gap.
* **G1 — inter-node data flow** (``schema_migration``):
  node 2 (migration) must agree with node 1 (schema) on a column set. Mode A's
  prompts read only ``${goal}`` so node 2 cannot see node 1's output and
  drifts; Mode A+B threads ``state["schema.columns"]`` so node 2 matches. Any
  A+B win here is 100% attributable because A *structurally cannot* thread.

Honesty / fairness rules encoded here
-------------------------------------
* The coder is a single deterministic function shared by both modes — the coder
  *capability* is identical; only the dispatch (fixed vs discovered fan-out,
  no-state vs state-threaded) differs. That isolates the variable.
* Mode A is given its best *honest* static config and we still show, via the
  fairness sweep in RESULTS, that no single fixed F wins across the K
  distribution — so the win is a paradigm gap, not a strawman.
* Determinism is intentional: it removes model variance so a single trial is
  conclusive, in deliberate contrast to the stochastic Sonnet n=10. We label
  every credited winner with its gap tier (G1 engine-fixable vs G3 paradigm).
"""

from .scenario import make_api_source, count_endpoints
from .coder_mock import add_validation, write_schema, write_migration
from .evaluator import coverage, consistency
from .modes import run_mode_a, run_mode_apb, FIXED_NODES, TOKENS_IN_PER_CALL, TOKENS_OUT_PER_CALL

__all__ = [
    "make_api_source",
    "count_endpoints",
    "add_validation",
    "write_schema",
    "write_migration",
    "coverage",
    "consistency",
    "run_mode_a",
    "run_mode_apb",
    "FIXED_NODES",
    "TOKENS_IN_PER_CALL",
    "TOKENS_OUT_PER_CALL",
]
