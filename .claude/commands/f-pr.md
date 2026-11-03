---
description: "/f-pr — use LLM judgment to pick the right dark-factory pipeline for an existing PR (gates only, full validation, or PR iteration) and run it with evidence"
type: skill
execution_mode: immediate
aliases: [pr-f, f-on-pr, factory-pr]
---

# /f-pr — drive an existing PR to /green via dark-factory

Explicit PR-mode entry point for the dark-factory binary-first contract —
skips the `/f` auto-route and always runs PR-mode. Read
`.claude/skills/dark-factory/SKILL.md` (this repo) and execute its `/f-pr` section:
PR-number resolution, PR-context reasoning (not a deterministic rule
table), pipeline/backend selection, and the shared proof-block output
contract.

## Usage

```
/f-pr                 # resolve PR from current branch
/f-pr 8123             # explicit PR number
```

## See also

- `/f` — auto-routes PR-mode vs feature-mode; `/f-pr` skips that route.
- `/factory` — alias for `/f`.
