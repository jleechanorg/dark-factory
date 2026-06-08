Explore the codebase before any design or implementation. Produce a durable
findings artifact the plan node will consume.

Goal:
${goal}

## Mandatory workflow (do not skip steps)

1. **Map concepts** — Identify the fields, flags, modules, routes, and state keys
   this goal touches. Grep/search the repo for every reference; list writers and
   readers with `path:line` citations.

2. **Authorities map** — For each concept, state which component is currently
   authoritative (or note conflicting authorities). Call out implicit state
   machines, legacy projection fields, god-mode paths, streaming vs non-streaming
   branches, and persistence boundaries.

3. **Reuse & centralization** — Surface existing modules, reducers, helpers, or
   patterns to extend instead of reimplementing. Propose where a single authority
   should live and which legacy surfaces become projections only.

4. **Risks & invariants** — List edge cases, race/finish-commit concerns, and
   invariants the design must preserve. Flag localized-patch temptations to
   reject.

## Output contract

Write `.dark-factory/explore-findings.md` in the target repo with these sections:

- **Concepts & grep coverage** (terms searched, files hit)
- **Writers / readers table** (concept → locations)
- **Current authorities** (who owns what today)
- **Centralization proposal** (recommended single authority + migration notes)
- **Reuse candidates** (existing code to extend)
- **Risks & invariants**

End your response with: `explore written: .dark-factory/explore-findings.md`

## Rules

- Read-only: do not modify production code or write `spec.md` in this node.
- Do not seek out, read, or summarize sealed holdout scenarios or evaluator
  internals; work only from the visible goal and repository.
- Do not propose implementation diffs yet — exploration and mapping only.
