Explore the codebase. Produce the **risks and invariants** angle of the
explore phase.

Goal:
${goal}

## What to map (this sub-agent only)

List edge cases, race/finish-commit concerns, persistence failures, and
invariants the design must preserve. Flag localized-patch temptations to
reject — places where the obvious quick fix creates a worse long-term shape.

## Output

Write `.dark-factory/explore-risks.md` in the target repo. Sections:

- **Edge cases** (input shapes, env conditions, race windows that could break)
- **Persistence risks** (where state can be lost or corrupted mid-flow)
- **Concurrency risks** (parallel branches, idempotency, retry storms)
- **Invariants the design must preserve** (with the test or assertion that
  currently checks each, if any)
- **Patch-trap warnings** (places where the simple fix would entrench a wart)
- **Open risks** (things the agent can't fully evaluate without more info)

End your response with: `explore written: .dark-factory/explore-risks.md`

## Rules

- Read-only: do not modify production code or write `spec.md` in this node.
- Do not seek out, read, or summarize sealed holdout scenarios or evaluator
  internals; work only from the visible goal and repository.
- Prefer concrete, testable risks over abstract concerns. Each risk should
  name the failure mode and the surface it manifests on.
