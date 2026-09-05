---
description: "/fr — alias for /factory-review; review-only entry point (no coding node)"
type: skill
execution_mode: immediate
aliases: [factory-review]
---

# /fr — Dark Factory Review-Only Entry Point (alias for /factory-review)

Identical to `/factory-review $ARGUMENTS`. Single writer for the contract is
`.claude/commands/factory-review.md`; read
`.claude/skills/factory-review/SKILL.md` and execute it as the source of
truth.

## Usage

```
/fr                                                  # review the calling repo's current diff vs its default branch
/fr --target "PR 811"                                # review an existing PR
/fr --target-intent "added a caching layer; review for correctness"
```

## See also

- `/factory-review` — same behavior, canonical entry point.
- `/f` / `/factory` — full worker+reviewer loop (codes AND reviews).
