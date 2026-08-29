---
description: "/f — full Dark Factory loop; auto-routes to PR-mode when a PR is open on the current branch, otherwise feature-mode. DEFAULT: invoke the real dark-factory binary and echo proof metadata."
type: skill
execution_mode: immediate
aliases: [f]
---

# /f — Full Dark Factory Loop

Single writer for the binary-first `/f`/`/factory` contract. Read
`.claude/skills/dark-factory/SKILL.md` (this repo) and execute it as the
source of truth — Step 0a (PR-mode vs feature-mode auto-route), Step 0b (CLI
backend auto-detect), Step 0c (pipeline select), reviewer calibration, and
the proof-block output contract all live there.

**Prerequisite:** `./install.sh` once; `dark-factory` on PATH via `~/.local/bin`.

## Usage

```
/f <goal description>                          # DEFAULT: binary-first factory run + reviewer calibration
/f max 5 rounds <goal>                         # explicit validation rounds bound (default: 3)
/f --max-rounds 5 <goal>                       # pass --max-rounds explicitly
/f --pipeline gates <goal>                     # explicit binary pipeline
/f --pipeline ready <goal>                     # explicit /ready validation pipeline
/f --pipeline minimal_research <goal>          # explicit research-only pipeline
/f --backend echo <goal>                       # wiring smoke (no LLM)
/f --feature <name> <goal>                     # override holdout feature key
/f --reviewer-calibration=false <goal>         # explicit opt-out; must explain why
/f --dynamic-graph <goal>                      # binary-owned dynamic DOT/workflow builder
/f --phase spec_validation <goal>              # binary-owned dynamic/phase run, if supported
```

## See also

- `/f-pr` — explicit PR-mode entry point (skips Step 0a auto-route).
- `/factory` — alias for `/f`, identical behavior.
- `/fs` — spec-generation entry point; run first when the goal needs a spec.
