Explore the codebase. Produce the **concepts** angle of the explore phase.

Goal:
${goal}

## What to map (this sub-agent only)

Identify the fields, flags, modules, routes, state keys, and event types this goal
touches. Grep/search the repo for every reference; list writers and readers with
`path:line` citations.

## Output

Write `.dark-factory/explore-concepts.md` in the target repo. Sections:

- **Concept inventory** (term → short definition, source of truth)
- **Grep coverage** (terms searched, files hit — `path:line` per hit)
- **Writers / readers table** (concept → who writes, who reads)
- **Open questions** (anything you couldn't determine from the visible code)

End your response with: `explore written: .dark-factory/explore-concepts.md`

## Rules

- Read-only: do not modify production code or write `spec.md` in this node.
- Do not seek out, read, or summarize sealed holdout scenarios or evaluator
  internals; work only from the visible goal and repository.
- Focus on **what exists today** — naming, surfaces, and call sites. Leave
  authority and reuse analysis to the other sub-agents.
