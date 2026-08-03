---
description: "/f — review any reviewable target with the default slim two-node Dark Factory graph"
type: skill
execution_mode: immediate
aliases: [f]
---

# /f — Dark Factory Review

Read `.claude/skills/dark-factory/SKILL.md` from the current repository when
available, otherwise the installed `~/.claude/skills/dark-factory/SKILL.md`,
and execute it as the source of truth. The active CLI must discover and use
the relevant installed/repository skills and policies. The controller prompt
remains the detailed review authority.

With no `--pipeline`, review any reviewable target — PR, commit, code, design
document, research report, evidence, docs, tests, or config — using
`pipelines/slim/two_node.dot`. The default is exactly one generic worker
followed by exactly one controller-owned Codex cold reviewer, with no shadow
reviewer. An explicit `--pipeline` is the only graph override.

**Prerequisite:** run `./install.sh` once so `dark-factory` is on `PATH` via
`~/.local/bin`.

## Usage

```
/f <review goal>                              # default slim two-node review
/f --pipeline gates <goal>                   # explicit graph override
/f --backend echo <goal>                     # wiring smoke (no LLM)
/f --feature <name> --pipeline gates <goal>  # feature key for a graph that requires it
```

## See also

- `/factory` — alias for `/f`, identical behavior.
- `/f-pr` — PR-oriented context gathering with the same graph default.
- `/fs` — explicit spec-generation entry point.
