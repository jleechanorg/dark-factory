Explore the codebase. Produce the **reuse and centralization** angle of the
explore phase.

Goal:
${goal}

## What to map (this sub-agent only)

Surface existing modules, reducers, helpers, or patterns that the upcoming
implementation should extend instead of reimplementing. Identify the highest-
value centralization proposals.

## Output

Write `.dark-factory/explore-reuse.md` in the target repo. Sections:

- **Reuse candidates** (existing code to extend — `path:line` + 1-line rationale)
- **Centralization proposal** (where a single authority should live)
- **Migration notes** (legacy surfaces that become projections only)
- **Anti-reuse traps** (patterns that look reusable but aren't safe to share)

End your response with: `explore written: .dark-factory/explore-reuse.md`

## Rules

- Read-only: do not modify production code or write `spec.md` in this node.
- Do not seek out, read, or summarize sealed holdout scenarios or evaluator
  internals; work only from the visible goal and repository.
- Be skeptical of "easy" reuse — call out any case where extending the
  existing code would entrench a wart, not clean it up.
