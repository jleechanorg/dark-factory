---
description: "/fs — generate main + attractor specs with codex cold review. DEFAULT: invoke the real dark-factory binary against spec_gen or a binary-owned dynamic spec graph, then echo proof metadata."
type: skill
execution_mode: immediate
aliases: [fs]
---

# /fs — Spec Generation (binary-first)

Read `.claude/skills/factory-spec/SKILL.md` and execute it as the source of
truth — create/review/show modes, Step 0 greenfield/brownfield
classification, pipeline selection, and the create-mode proof block all
live there.

`/fs` produces **two** reviewed specs by default: `spec.md` (main —
acceptance criteria, test command, non-goals, lane-independence matrix) and
`attractor_spec.md` (convergence target, anti-attractor states), both signed
off by an independent cold codex review before exit.

## Usage

```text
/fs <spec description>                  # DEFAULT: binary spec_gen run (main + attractor)
/fs --pipeline spec_gen <description>   # explicit static binary pipeline
/fs --dynamic-graph <description>       # binary-owned dynamic spec graph
/fs --skip-attractor <description>      # main spec only, still binary-first
/fs --review <spec_path>                # in-session review of an existing main spec
/fs --review-attractor <path>           # in-session review of an existing attractor spec
/fs --show                              # read-only pipeline graph reference
```

## See also

- `/factory-spec` — same skill, alternate entry point.
- `/f` — full Dark Factory loop; run after specs are reviewed.
