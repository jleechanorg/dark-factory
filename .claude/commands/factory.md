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
/factory <goal>                          # auto-detect: PR-mode or feature-mode; reviewer calibration on
/factory max 5 rounds <goal>             # explicit validation rounds bound (default: 3)
/factory --max-rounds 5 <goal>           # pass --max-rounds explicitly
/factory --pipeline gates <goal>         # explicit pipeline (skips auto-detect)
/factory --pipeline ready <goal>         # explicit /ready validation pipeline
/factory --pipeline minimal_research <goal> # explicit research-only pipeline
/factory --backend echo <goal>           # dry-run wiring smoke
/factory --feature hello <goal>          # override holdout feature key
/factory --reviewer-calibration=false <goal>  # explicit opt-out; same as /f
```

## See also

- `/f` — same behavior, canonical entry point.
- `.claude/skills/dark-factory/SKILL.md` — full contract (Step 0a/0b/0c,
  reviewer calibration, proof-block output contract).
