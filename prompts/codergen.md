# Codergen Prompt

Generic, lane-agnostic instruction for any `type="codergen"` node. The runner
substitutes the following tokens into your prompt template at dispatch time:

- `${goal}` — the top-level goal string passed to the pipeline (e.g. via
  `--goal`, or the `graph [goal="..."]` attribute). Always present.
- `${state.*}` — any key under `state`, e.g. `${state._last_output}`,
  `${state.branch_a.outcome}`. Use these to read prior node outputs, hints,
  or shared context. Missing keys render as empty strings.

## Your job

1. Read the `${goal}` and decide what concrete artifact this node must
   produce (a file edit, a code review, a research report, a fix, etc.).
2. Inspect `state.*` for prior node outputs, shared context, hints, or
   artifacts you should build on. A typical pattern is to read
   `${state.<upstream_node>.output_head}` (a one-line summary written by
   the previous node) for orientation, then dig into the full artifact
   in `.dark-factory/` or wherever the upstream node wrote it.
3. Do the work. Stay focused on the node's slice of the goal — do not
   expand scope into neighboring nodes' responsibilities.
4. Emit a `Result` with:
   - `outcome` — one of `success`, `failure`, `error`.
   - `output_head` — a 1–3 line summary of what you produced, suitable
     for downstream nodes to read via `${state.<this_node>.output_head}`.
   - On `failure` or `error`: include the failure reason in `metadata`
     (e.g. `metadata: { "reason": "missing input file spec.md" }`) so the
     Healer can cluster terminal failures by cause.

## Conventions

- Concise. This is a generic prompt; pipelines that need richer
  instructions should ship their own `prompts/<lane>/<node>.md` and
  reference it from the `.dot` (e.g. `prompt="@prompts/slim/plan.md"`).
- Do not write hidden holdout scenarios, evaluator internals, or sealed
  test sources into any artifact. Those live in the sealed holdouts
  repo and must not be read or paraphrased.
- Keep the diff focused. Record changed files and evidence in your final
  response so the gate nodes (evidence review, code standards) can audit.
