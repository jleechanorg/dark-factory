Explore the codebase. Produce the **authorities** angle of the explore phase.

Goal:
${goal}

## What to map (this sub-agent only)

For every concept relevant to this goal, state which component is currently
authoritative. Call out:

- Implicit state machines (where state lives across multiple files)
- Legacy projection fields (read-only mirrors, deprecated aliases)
- God-mode paths (handlers that bypass the normal flow)
- Streaming vs non-streaming branches
- Persistence boundaries (where authoritative state diverges from cached state)

## Output

Write `.dark-factory/explore-authorities.md` in the target repo. Sections:

- **Current authorities** (concept → owning component + `path:line`)
- **Conflicting authorities** (places where two components both claim write)
- **Implicit state machines** (state key → lifecycle, persistence, readers)
- **Streaming / non-streaming branches** (call sites that diverge)
- **God-mode paths** (any code that bypasses the normal pipeline)

End your response with: `explore written: .dark-factory/explore-authorities.md`

## Rules

- Read-only: do not modify production code or write `spec.md` in this node.
- Do not seek out, read, or summarize sealed holdout scenarios or evaluator
  internals; work only from the visible goal and repository.
- Focus on **who owns what today** — leaving reuse/centralization to the
  reuse sub-agent.
