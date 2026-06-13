// prompts/slim/explore.md
//
// NOTE: This prompt was repurposed in the PR #22 follow-up. It used to be
// the "explore — write explore-findings.md directly" prompt (a single agent
// that wrote the consolidated artifact). After the 4-way parallel explore
// fanout shipped in _base.dot, the 4 sub-agents each write a partial
// (`explore-concepts.md`, `explore-authorities.md`, `explore-reuse.md`,
// `explore-risks.md`) and this node stitches them into the consolidated
// `.dark-factory/explore-findings.md` that `prompts/slim/plan.md` requires.
//
// No lane references `prompt="@prompts/slim/explore.md"` for the old direct
// explore path anymore; only the `_base.dot` `explore_stitch` node does.

Stitch the four parallel-explore partial files into a single consolidated
findings artifact the plan node will consume.

Goal:
${goal}

## Input (must read all four)

- `.dark-factory/explore-concepts.md`
- `.dark-factory/explore-authorities.md`
- `.dark-factory/explore-reuse.md`
- `.dark-factory/explore-risks.md`

If any of the four is missing, stop and report which file is missing.
Do not invent a design from scratch.

## Output (must write)

`.dark-factory/explore-findings.md` in the target repo, with these four
sections in this order. Each section is the verbatim content of the
corresponding partial, prefixed by a level-2 heading:

- `## Authors / Authorities` — copy from `.dark-factory/explore-authorities.md`
- `## Concepts` — copy from `.dark-factory/explore-concepts.md`
- `## Reuse` — copy from `.dark-factory/explore-reuse.md`
- `## Risks` — copy from `.dark-factory/explore-risks.md`

After the four sections, append a one-line summary derived ONLY from the
content of the four partials (no new claims, no new design).

## Rules (load-bearing)

- The four partials are the only source of truth. Do not invent new content.
- Do not modify production code or write `spec.md` in this node.
- Do not seek out, read, or summarize sealed holdout scenarios or evaluator
  internals; work only from the four partials and the visible goal.
- If two partials disagree, surface the disagreement under the relevant
  section but do not resolve it.
- If the goal is infeasible (due to a non-existent target repo, already-implemented feature, or forbidden action class), do not write the four sections. Instead, explain clearly why the goal is infeasible and end your response with: `explore stitched: early_exit`.

End your response with: `explore stitched: .dark-factory/explore-findings.md` or `explore stitched: early_exit` depending on feasibility.
