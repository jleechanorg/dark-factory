---
description: "/factory — alias for /f; defaults to one worker plus one controller-owned Codex cold reviewer"
type: skill
execution_mode: immediate
aliases: [df]
---

# /factory — Dark Factory DOT Pipeline Runner (alias for /f)

Identical to `/f $ARGUMENTS`. Single writer for the binary-first contract is
`.claude/commands/f.md`; read `.claude/skills/dark-factory/SKILL.md` and
execute it as the source of truth.

## Usage

```
/factory <goal>                          # worker + fixed Codex cold reviewer
/factory --pipeline gates <goal>         # explicit pipeline (skips auto-detect)
/factory --backend echo <goal>           # dry-run wiring smoke
/factory --feature hello <goal>          # override holdout feature key
/factory --reviewer-calibration=true <goal>   # explicit extra calibration lanes
```

## See also

- `/f` — same behavior, canonical entry point.
- `.claude/skills/dark-factory/SKILL.md` — full contract (fixed default,
  explicit opt-ins, proof-block output contract).
