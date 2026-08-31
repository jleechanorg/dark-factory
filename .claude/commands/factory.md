---
description: "/factory — alias for /f; auto-routes to PR-mode when a PR is open, otherwise feature-mode"
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
/factory <goal>                          # default: SlimTwoNode + fresh fully tooled Codex reviewer
/factory --pipeline gates <goal>         # explicit pipeline (skips auto-detect)
/factory --backend echo <goal>           # dry-run wiring smoke
/factory --feature hello <goal>          # override holdout feature key
/factory --reviewer-calibration=true <goal>   # explicit calibration workflow
```

## See also

- `/f` — same behavior, canonical entry point.
- `.claude/skills/dark-factory/SKILL.md` — full contract (Step 0a/0b/0c,
  reviewer calibration, proof-block output contract).
