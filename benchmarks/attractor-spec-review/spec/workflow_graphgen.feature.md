# Workflow-Generated Graph Mode and A-vs-A+B Benchmark

## Goal

Define a new default graph-construction mode for the Dark Factory in which a
Claude Workflow generates a pipeline graph for each goal, and define a benchmark
that compares two ways of executing that generated graph against sealed holdouts.

## User story

As an operator running the factory, I want the pipeline graph to be synthesized
for my specific goal with mandatory reviewer nodes always present, and I want
evidence from sealed holdouts telling me whether live Workflow execution beats
the deterministic runner before I adopt either approach as the default.

## Inputs

- A goal string describing the feature to build.
- A feature key naming the sealed holdout used to grade the result.
- A resolved coder backend identifier and model name.
- The existing runner, parser, and handler code in this repository.

## Definitions

- Generator Workflow means the harness orchestration tool running on Opus that
  reads the goal and returns a graph description.
- Dynamic middle means the work nodes between start and the guaranteed reviewers.
- Guaranteed nodes means the reviewer nodes injected into every generated graph.
- Mode A means the runner walks every generated node, including the middle.
- Mode A plus B means the Workflow runs the middle and the runner runs the tail.

## Mode A behavior

The generator emits one complete graph file. The Dark Factory runner walks the
graph from start to exit. Every node, including the dynamic middle, runs through
the deterministic Python runner using the resolved coder backend.

## Mode A plus B behavior

The generator emits a pinned spine graph that contains only start, the guaranteed
reviewer nodes, and exit, plus the same dynamic middle description. The Workflow
runs each middle node as a coder agent call, commits the combined diff onto the
recorded baseline reference, then runs the Dark Factory runner on the spine graph
so the guaranteed reviewers grade the diff through the same runner as Mode A.

## Independent variable and symmetric reviewer wiring

The only permitted difference between the two modes is which executor runs the
dynamic middle. To keep that the sole difference, both guaranteed reviewer nodes
run as terminal non-retrying gates in the benchmark: a failing verdict is recorded
and routes to exit, never back to a fix node. This matters because the Mode A plus
B spine has no fix node, since the dynamic middle and any fix loop already ran in
the Workflow and were committed before the spine runs. If the reviewers retried
into fix, Mode A could repair a failing review while Mode A plus B could not,
which would corrupt the conformance and robustness axes. The dynamic middle may
still contain its own internal fix loop in both modes, because that loop is part
of the shared graph description and is therefore identical across modes.

## Generator output contract

The generator returns a graph description object with five fields. The nodes field
lists work nodes, each carrying a name, a type drawn from the allowed vocabulary,
a prompt path from the catalog, and an optional backend and model name. The edges
field lists directed edges, each carrying a source, a target, and a condition
string. The guaranteed field lists the pinned reviewer nodes. The rationale field
carries one sentence explaining the chosen shape. The vocabulary field repeats the
allowed node types.

## Allowed dynamic node vocabulary

The generator may only choose node types from this fixed set: plan, implement,
test, fix, review, refactor, research, and stack smoke. The generator may not
invent any node type outside this set, which keeps generation bounded and easy
to audit.

## Prompt catalog requirement

Each of the eight vocabulary node types maps to exactly one existing prompt
template under the prompts directory, listed in a catalog file. The generator may
only reference paths that appear in that catalog. Before any agent runs in either
mode, the benchmark harness checks that every generated node prompt path exists on
disk and appears in the catalog, and fails the run during validation when a path
is missing. The catalog must cover all eight node types so a generated graph that
uses refactor, research, or stack smoke never points at an absent file.

## Graph validity requirements

Every generated graph must contain a start node and an exit node. The start node
must have no incoming edges. The exit node must have no outgoing edges. Every node
must be reachable from start. These rules match what the runner parser enforces,
so a generated graph that breaks them is rejected before any agent runs.

## Guaranteed node one

The first guaranteed node is the cold code reviewer. It runs through an
independent codex process that has never read the implementation prompt and sees
only the spec and the committed diff. It is wired as a terminal node, meaning the
goal gate attribute is unset and the node has one unconditional edge to exit, so a
failing verdict is recorded and the run terminates. The goal gate attribute must
not be enabled here, because in the engine an enabled goal gate is the retry on
failure trigger that routes an unsuccessful node to a retry target, which is the
asymmetry the symmetric wiring section forbids. This node also emits the
structured graph quality score.

## Guaranteed node two

The second guaranteed node is the evidence reviewer. It runs the evidence
standards gate and then the evidence review gate against the produced diff as
terminal nodes with no goal gate and an unconditional edge onward to exit,
enforcing the repository evidence rules. Both guaranteed nodes appear in every
generated graph in both modes. Because the engine also honors a graph level retry
target, the benchmark harness asserts in both modes that neither guaranteed
reviewer node nor the graph enables a goal gate or declares a retry target.

## Coder backend support

The coder backend is a benchmark parameter. The default coder is the claude
command pinned to the Sonnet model. Native runner branches cover claude, codex,
and antigravity. Every other Agent Orchestrator plugin, namely gemini, cursor,
aider, opencode, and minimax, is reached through the orchestrator backend by
naming the plugin, which delegates to an orchestrator spawn with that agent.

## Backend fairness scope

The fair head to head comparison is restricted to the claude backend pinned to
Sonnet, because Mode A plus B runs the dynamic middle through harness agent calls
that execute the same claude Sonnet coder the runner uses in Mode A. For that
backend both modes use an identical coder model, so any cost or conformance
difference is attributable to the executor. The other backends are supported only
in Mode A and are labeled exploratory, because the Workflow agent call cannot run
a non claude coder for the middle without changing the model and breaking parity.
The report marks exploratory rows and excludes them from winner aggregation.

## Model pin prerequisite

The runner already resolves the backend from a node attribute named model when no
explicit backend is set, which treats a bare model attribute as a backend alias.
To avoid routing a Sonnet pin to a nonexistent backend named after the model
string, the new coder model attribute is named model name instead of model. The
claude backend branch reads the model name attribute and passes it to the command
as a model flag when present, and the existing alias behavior is left unchanged.
A regression test asserts the flag appears in the built argument list when the
attribute is set and is absent when it is not, and a second test asserts a node
that sets the model name attribute without a backend still dispatches to the
claude backend. This change lets the benchmark pin Sonnet on the coder.

## Diff handoff contract

The benchmark harness records the worktree head as the baseline reference before
any middle execution, identically for both modes. In Mode A plus B the Workflow
commits the combined middle diff as one commit on top of the baseline reference
before invoking the runner on the spine. The guaranteed reviewers and the sealed
evaluator diff the current head against the baseline reference in both modes, so
each grades the same non empty diff produced from the same baseline. A run whose
diff against the baseline reference is empty is recorded as a failed run.

## Benchmark corpus

The corpus is four sealed holdout features chosen to span difficulty. The hello
feature is the wiring control. The roman feature is the single file algorithmic
control. The conclude finalize feature is the medium multi step goal. The airbnb
clone sprint one feature is the full stack discriminator.

## Trials

Each feature runs three trials in each mode to sample model nondeterminism. The
benchmark therefore runs twenty four implementation attempts in total in the fair
claude Sonnet lane.

## Scored axes

Each run produces one record with four kinds of measurement. The conformance
measurement reads the pass count and total from the sealed evaluator aggregate
line. The cost measurement records input tokens, output tokens, and wall clock
milliseconds accounted on equal footing. The graph quality measurement reads the
structured score described below. The robustness measurement records whether the
run reached exit with no human help and whether the loop bounds held, and is
emitted under the field name zero touch to match the main spec.

## Graph quality scoring

The graph quality score grades the shared graph description that is generated once
per goal, not the rendered graph file. The deterministic seventy percent, made of
presence and edge validity, is identical across modes by construction. The thirty
percent node selection fit part is a live reviewer judgment, so it is scored once
per goal on the shared description and reused for both modes rather than queried
again per mode, which keeps the whole axis mode invariant instead of letting
reviewer sampling noise leak an executor difference. It is a zero to one hundred
number built from three weighted parts. The guaranteed node presence part, weight
thirty five percent, is computed in code as a binary check that both reviewer
nodes are present and reachable. The edge validity part, weight thirty five
percent, is computed in code as a binary check that the description renders to a
parser valid graph. The node selection fit part, weight thirty percent, is a zero
to one hundred judgment from the cold reviewer using a fixed rubric prompt
at prompts/graph_quality_rubric.md. When the reviewer score cannot be parsed, the fit part is recorded
as unscored and the run reports the deterministic seventy percent partial with an
unscored flag rather than a silent zero.

## Token accounting per mode

Both modes count coder execution tokens only for the dynamic middle, read from the
same field for the same backend. The Sonnet coder reports input and output tokens,
and both modes read those identical fields, Mode A from the event database per
node records and Mode A plus B from the agent result for the matching middle node.
The Opus generator runs once per goal and is shared by both modes, so its
generation tokens are recorded separately and excluded from the per mode middle
cost on both sides because they are equal by construction. Orchestration overhead
that is not coder execution is excluded from both modes. A unit test asserts both
modes pull the identical token fields for the claude backend.

## Fairness controls

Both modes share the goal string, the feature key, the coder backend, the model,
the guaranteed nodes, the baseline reference, the sealed evaluator, and the trial
count. The graph description is generated once per goal and shared by both modes
so the dynamic middle entering each mode is identical. The only permitted
difference is which executor runs the dynamic middle.

## Aggregation and verdict

Results are aggregated as mean and range over the three trials per feature. The
benchmark declares a per axis winner only when the two modes trial ranges do not
overlap on that axis, and reports overlapping ranges as no separation at this
sample size rather than a winner. The benchmark result is an outcome artifact that
informs which mode becomes the default graph construction strategy.

## Acceptance contract

The work is accepted when the generator produces parser valid graphs for all four
corpus features in both modes and every node prompt path resolves against the
catalog, when both model pin regression tests pass, when one record is emitted per
run for all twenty four runs in the fair lane, when the aggregation yields per
axis ranges and either a non overlapping winner or a no separation result, when at
least one mode reaches the full pass count on both control features in at least
two of three trials or the result is recorded as inconclusive, and when the cold
reviewer findings on this spec are resolved as evidenced by a saved review report
with a pass verdict at a named path.
