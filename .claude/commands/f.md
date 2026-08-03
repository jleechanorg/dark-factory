---
description: "/f — run one generic worker and one controller-owned Codex cold reviewer by default. Extra pipelines and review lanes are explicit opt-ins."
type: skill
execution_mode: immediate
aliases: [f]
---

# /f — Full Dark Factory Loop

Single writer for the binary-first `/f`/`/factory` contract. Read
`.claude/skills/dark-factory/SKILL.md` (this repo) and execute it as the
source of truth — backend selection, the fixed two-node default, explicit
pipeline opt-ins, and the proof-block output contract all live there.

**Prerequisite:** `./install.sh` once; `dark-factory` on PATH via `~/.local/bin`.

## Usage

```
/f <goal description>                          # DEFAULT: worker + fixed Codex cold reviewer
/f --pipeline gates <goal>                     # explicit binary pipeline
/f --backend echo <goal>                       # wiring smoke (no LLM)
/f --feature <name> <goal>                     # override holdout feature key
/f --reviewer-calibration=true <goal>          # explicit extra calibration lanes
/f --dynamic-graph <goal>                      # binary-owned dynamic DOT/workflow builder
/f --phase spec_validation <goal>              # binary-owned dynamic/phase run, if supported
```

## See also

- `/f-pr` — explicit PR-mode entry point.
- `/factory` — alias for `/f`, identical behavior.
- `/fs` — spec-generation entry point; run first when the goal needs a spec.
