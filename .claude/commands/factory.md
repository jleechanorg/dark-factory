---
description: "/factory — alias for /f; review any reviewable target with the default slim two-node graph"
type: skill
execution_mode: immediate
aliases: [df]
---

# /factory — Dark Factory Review (alias for /f)

Follow `.claude/commands/f.md`, then read
`.claude/skills/dark-factory/SKILL.md` from the current repository when
available or the installed `~/.claude/skills/dark-factory/SKILL.md`. The active
CLI must discover and use the relevant installed/repository skills and
policies. The controller prompt remains the detailed review authority.

With no `--pipeline`, review any reviewable target — PR, commit, code, design
document, research report, evidence, docs, tests, or config — using
`pipelines/slim/two_node.dot`. The default is exactly one generic worker
followed by exactly one controller-owned Codex cold reviewer, with no shadow
reviewer. An explicit `--pipeline` is the only graph override.

## Usage

```
/factory <review goal>                              # default slim two-node review
/factory --pipeline gates <goal>                   # explicit graph override
/factory --backend echo <goal>                     # wiring smoke (no LLM)
/factory --feature <name> --pipeline gates <goal>  # feature key for a graph that requires it
```

## See also

- `/f` — canonical entry point with identical behavior.
- `.claude/skills/dark-factory/SKILL.md` — detailed execution contract.
