# Spec: Workflow-Generated Graphs + A-vs-A+B Benchmark (`workflow_graphgen`)

Status: cold-reviewer PASS (iteration 3) — see spec_review/workflow_graphgen_reviewer.json
Classification: greenfield (net-additive new mode + benchmark harness)
Owner: operator
Source design: brainstormed 2026-06-04, this repo

## 1. Purpose

Add a new **default graph-construction mode** to `/f` / `/factory` in which a
Claude **Workflow** (the harness orchestration tool, Opus) *generates* a pipeline
graph per goal, instead of selecting a hand-written `.dot` from a fixed catalog.
The generated graph pins a small set of **guaranteed reviewer nodes** while the
work nodes in the middle are chosen dynamically for the goal.

We then **benchmark** two strategies for executing that generated graph against
sealed holdouts, so the choice is decided by outcome artifacts rather than taste.

## 2. The two modes under test

- **Mode A — generator only.** The generator Workflow emits a complete `.dot`.
  The existing `dark-factory` Python runner walks every node, including the
  dynamic middle, using the resolved coder backend.
- **Mode A+B — generator plus hybrid runtime.** The generator emits a
  pinned-spine `.dot` containing only the guaranteed reviewer nodes, plus the
  same dynamic-middle graph-IR. The Workflow runs the dynamic middle itself via
  `agent()`, commits the resulting diff to a known ref (§7a), then shells out to
  `dark-factory` on the spine `.dot` so the guaranteed reviewer nodes run through
  the same deterministic runner as Mode A.

The guaranteed-node guarantee is identical in both modes. The single independent
variable is **who executes the dynamic middle**: the Python runner (A) versus the
live Opus Workflow (A+B). §3a, §5, §7a, and §9a remove the confounds that would
otherwise let a winner be declared for any other reason.

## 3. Roles and isolation

The agent isolation rules in `CLAUDE.md` are preserved without exception. The
generator Workflow and the dynamic-middle agents are implementing agents: they
read `specs/`, `prompts/`, and the graph, and they never read holdout scenarios,
the evaluator, or any holdout source. A separate evaluator agent runs the sealed
evaluator out-of-band and grades the diff blind; its location is operator-only
and is deliberately not referenced here.

## 3a. Symmetric reviewer wiring (no repair-loop confound)

In the benchmark, both guaranteed reviewer nodes run as **terminal, non-retrying
gates**: a failing verdict records the verdict and routes to `exit`, never back to
a `fix` node. This is required because the Mode A+B spine has no `fix` node (the
dynamic middle, including any `fix`, was already executed by the Workflow and
frozen into the committed diff before the spine runs). If the guaranteed reviewers
retried into `fix`, Mode A could self-repair on a failing review while Mode A+B
could not, confounding the conformance and `zero_touch` axes. Terminal gates make
the two modes symmetric: in both modes the diff entering the guaranteed reviewers
is final, and the reviewer verdict is a measured outcome, not a repair trigger.

The dynamic middle may still contain its own internal `fix` loop in **both** modes
(runner-driven in A, `agent()`-driven in A+B); that loop is part of the shared
graph-IR and is therefore not an asymmetry.

## 4. Generator contract

The generator Workflow receives the goal text, the feature key, and the resolved
coder backend. It returns a graph-IR object with these fields:

- `nodes`: array of work-node descriptors, each with `name`, `type` from the
  allowed vocabulary, `prompt` path (from the catalog in §4a), and optional
  `backend` / `model_name`.
- `edges`: array of `{from, to, condition}` records using the runner edge syntax.
- `guaranteed`: the pinned reviewer nodes the generator must always include.
- `rationale`: one line explaining why this shape fits the goal.

The allowed dynamic-node vocabulary is fixed: `plan`, `implement`, `test`, `fix`,
`review`, `refactor`, `research`, `stack_smoke`. The generator may not invent
node types outside this list. Generation is therefore bounded and auditable.

The graph-IR renders to a `.dot` that satisfies the parser: it contains both
`start` and `exit`, the start node has no incoming edges, the exit node has no
outgoing edges, and every node is reachable from start.

## 4a. Prompt catalog (every node type resolves before any agent runs)

Each of the eight vocabulary types maps to exactly one existing prompt template
under `prompts/`, listed in a catalog file `prompts/catalog.json`. The generator
may only reference paths in that catalog; it may not emit a free-form prompt path.
Before any agent runs in either mode, the benchmark harness validates that every
generated node's `prompt` path exists on disk and is present in the catalog; an
unresolved path fails the run during validation rather than at render time. The
catalog must cover all eight types, including `refactor`, `research`, and
`stack_smoke`, so a generated graph that uses them never references a missing file.

## 5. Guaranteed nodes (always injected, never model-chosen)

1. `code_reviewer`: an independent cold reviewer invoked through
   `codex exec --yolo`, which has never seen the implementation prompt and reads
   only the spec and the committed diff. In the benchmark it is wired as a
   **terminal gate** per §3a. Terminal means concretely: `goal_gate` is unset (or
   false) and the node has a single unconditional edge to `exit`, so a failing
   verdict is recorded and the run terminates. `goal_gate=true` must **not** be
   used here — in `runner/engine.py:_goal_gate_target` it is the retry-on-failure
   trigger that routes an unsuccessful node to a `retry_target`, which is exactly
   the asymmetry §3a forbids. This node also emits the structured graph-quality
   score defined in §9a.
2. `evidence_reviewer`: the evidence gates `gate_es` then `gate_er`, enforcing
   the repository evidence standards on the produced diff, also as terminal gates
   (no `goal_gate`, unconditional edge onward to `exit`).

Because `_goal_gate_target` also honors a **graph-level** `retry_target`, "no
retry edge on the node" is necessary but not sufficient. The benchmark harness
must assert, in **both** modes, that neither guaranteed reviewer node nor the
graph declares `goal_gate=true` or any `retry_target`, so Mode A cannot quietly
gain a repair loop the Mode A+B spine lacks.

These two nodes appear in every generated graph in both modes. `holdout_eval` is
the shared scoring anchor and is appended by the benchmark harness around both
modes rather than chosen by the generator.

## 6. Coder backend support

The coder backend is a benchmark parameter. The default coder is the `claude`
CLI pinned to `claude-sonnet-4-6` through the `--model` passthrough added in §7.
The orchestrator is Opus in both modes by virtue of running in this harness.

Native runner branches cover `claude`, `codex`, and `agy`. All remaining Agent
Orchestrator coder plugins (`gemini`, `cursor`, `aider`, `opencode`, `minimax`)
are reached through the `ao` backend by setting `agent="<plugin>"`, which
delegates to `ao spawn --agent <plugin>`. The benchmark CLI accepts
`--coder-backend {claude|codex|agy|ao:<plugin>}`.

### 6a. Backend fairness scope

The **fair, headline A-vs-A+B comparison is restricted to `--coder-backend
claude` pinned to Sonnet**, because Mode A+B runs the dynamic middle through
harness `agent()` calls, which execute the same `claude`/Sonnet coder the runner
uses in Mode A. For that backend the two modes use an identical coder model, so a
cost or conformance difference is attributable to the executor.

Other backends (`codex`, `agy`, `ao:<plugin>`) are supported only in **Mode A**
and are labeled **exploratory**: they exercise the generator and runner across
coder CLIs but are not part of the head-to-head verdict, because A+B's `agent()`
cannot invoke a non-`claude` coder for the middle without changing the coder
model and breaking parity. The benchmark report marks exploratory rows explicitly
and excludes them from per-axis winner aggregation.

## 7. Prerequisite: `--model` passthrough for the `claude` backend

The `claude` backend branch currently calls the CLI without a `--model` flag, so
it inherits the CLI default model (Opus). This spec requires a small, **named to
avoid collision** change.

`runner/handlers.py` already resolves the coder backend at the line
`backend = node.attrs.get("backend", node.attrs.get("model", ctx.backend))`,
which treats a bare `model` attribute as a *backend alias*. A node that set
`model="claude-sonnet-4-6"` with no explicit `backend` would therefore be routed
to a nonexistent backend named after the model string, not to the `claude`
branch. To avoid this collision the new attribute is named **`model_name`**, not
`model`:

- The `claude` backend branch reads an optional `model_name` node attribute and,
  when present, passes `--model <value>` to the CLI.
- The existing `model`-as-backend-alias behavior at the resolution line is left
  unchanged, so no current pipeline regresses.

Regression tests assert: (1) `--model <value>` appears in the constructed
argument list when `model_name` is set and is absent when it is not; and (2) a
node that sets `model_name` but no `backend` still dispatches to the `claude`
backend (not a backend named after the model string). Without this change the
benchmark cannot pin Sonnet on the coder.

## 7a. Diff handoff contract (Mode A+B)

Mode A+B must present the dynamic-middle diff to the spine reviewers through the
**same mechanism and baseline ref** Mode A uses. The contract:

1. The benchmark harness records the worktree HEAD as `baseline_ref` before any
   middle execution, identically for both modes.
2. In Mode A+B the Workflow **commits** the combined middle diff as a single
   commit on top of `baseline_ref` before invoking `dark-factory` on the spine.
3. The guaranteed reviewers and the sealed evaluator diff the current HEAD against
   `baseline_ref` in both modes, so each grades the same non-empty diff produced
   from the same baseline. A run whose diff against `baseline_ref` is empty is
   recorded as a failed run, not a passing one.

## 8. Benchmark corpus and trials

The corpus is four holdout features spanning difficulty tiers: `hello` as a
wiring control, `roman` as a single-file algorithmic control, `conclude-finalize`
as a medium multi-step goal, and `airbnb-clone-sprint-1` as a full-stack
discriminator. Each feature runs three trials per mode to sample model
nondeterminism, for twenty-four runs total in the fair (`claude`/Sonnet) lane.

## 9. Scoring across four axes

Each run produces one JSON record with these measurements:

- `conformance`: pass count and total from the sealed evaluator aggregate line.
- `tokens_in`, `tokens_out`, `wall_ms`: cost to reach the terminal verdict,
  accounted per §9b.
- `graph_quality`: the structured 0–100 score defined in §9a.
- `zero_touch`: a boolean for reaching exit with no human intervention, plus an
  intervention count and a record of whether loop bounds held. This is the
  canonical field name for the fourth axis in both specs and in the JSON schema.

Results are aggregated as mean and range over the three trials per feature.

## 9a. Graph-quality scoring (deterministic where possible)

`graph_quality` scores the **shared graph-IR** generated once per goal, not the
rendered `.dot`. The deterministic 70% (presence + edge validity) is therefore
bit-identical across modes and cannot differ on executor. The 30%
`node_selection_fit` part is a live reviewer judgment, so to keep the whole axis
mode-invariant it is **scored once per goal on the shared IR and reused for both
modes** rather than re-queried per mode (a re-query would let reviewer sampling
noise leak an executor difference into an identical IR). It is a 0–100 number
composed of three weighted sub-scores:

- `guaranteed_node_presence` (weight 0.35): computed **in code**, binary 0 or
  100 — both guaranteed reviewer nodes present and reachable from start.
- `edge_validity` (weight 0.35): computed **in code**, binary 0 or 100 — the IR
  renders to a parser-valid `.dot` (start/exit present, start no-incoming, exit
  no-outgoing, all nodes reachable, every edge condition well-formed).
- `node_selection_fit` (weight 0.30): a 0–100 judgment from the cold reviewer of
  how well the chosen middle node types fit the goal, using a fixed rubric prompt
  stored at `prompts/graph_quality_rubric.md`. On reviewer parse failure or
  timeout this sub-score is recorded as `null` and the run's `graph_quality` is
  reported as the deterministic 0.70-weighted partial plus a `node_fit:unscored`
  flag, never silently zeroed.

`graph_quality = 0.35*presence + 0.35*edge_validity + 0.30*node_selection_fit`.

## 9b. Token accounting parity

Both modes count **coder-execution tokens only** for the dynamic middle, read
from the same field for the same backend. Specifically:

- The Sonnet/`claude` coder reports input and output tokens; both modes read
  those identical fields (Mode A from CXDB per-node records, Mode A+B from the
  `agent()` result for the corresponding middle node), so the categories match.
- The Opus **generator** Workflow runs **once per goal** and is shared by both
  modes; its generation tokens are recorded separately as `gen_tokens` and are
  **excluded** from the per-mode middle cost on both sides, since they are equal
  by construction and would cancel.
- Orchestration overhead (the harness/runner glue that is not coder execution) is
  excluded from both modes.

A unit test asserts both modes' accounting pulls the identical token fields for
the `claude` backend so a cost winner reflects executor efficiency, not
measurement skew.

## 10. Fairness controls

Both modes use the same goal text, the same feature key, the same coder backend
and model, the same guaranteed nodes, the same baseline ref (§7a), the same sealed
evaluator, and the same trial count. The graph-IR is generated once per goal and
shared by both modes so the dynamic middle is identical going in. The only
difference permitted between the two modes is the executor of the dynamic middle.

## 11. Acceptance

The feature is accepted when:

1. The generator produces parser-valid graphs for all four corpus features in
   both modes, and every generated node's `prompt` path resolves against the §4a
   catalog.
2. The `--model` passthrough tests pass, including the no-`backend` dispatch test
   in §7.
3. A full benchmark run in the fair `claude`/Sonnet lane emits one JSON record per
   run for all twenty-four runs, using the §9 schema with the canonical
   `zero_touch` field name.
4. The aggregation produces a per-axis and overall result, reported as a
   **directional signal** with mean and range: a per-axis winner is declared only
   when the two modes' trial ranges do not overlap on that axis; overlapping
   ranges are reported as "no separation at n=3" rather than a winner. (Three
   trials sample nondeterminism but are too thin to call a close axis.)
5. A substantive conformance bar holds: at least one mode reaches the full pass
   count on both control features (`hello` and `roman`) in at least two of three
   trials; otherwise the benchmark result is recorded as **inconclusive** rather
   than accepted.
6. The cold reviewer's findings on this spec and the attractor spec are resolved,
   evidenced by a saved review report with `verdict: pass` at
   `spec_review/workflow_graphgen_reviewer.json`.
