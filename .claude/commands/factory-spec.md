---
description: "/factory-spec — create or review a Dark Factory spec (main + attractor) with codex cold review of both"
type: skill
execution_mode: immediate
aliases: []
---

# /factory-spec — Two-Phase Spec Generation (Main + Attractor)

Read `.claude/skills/factory-spec/SKILL.md` and execute it as the source of
truth. Identical workflow to `/fs`; this is an alternate entry point name.

## Usage

```text
/factory-spec <spec description>          # create mode — runs spec_gen (main + attractor)
/factory-spec --review <spec_path>        # in-session review of an existing main spec
/factory-spec --review-attractor <path>   # in-session review of an existing attractor spec
/factory-spec --skip-attractor <desc>     # create mode but skip the attractor phase
/factory-spec --show                      # show pipeline graphs (read-only reference)
```

## See also

- `/fs` — same skill, shorter alias.
- `/f` — full Dark Factory loop; run after specs are reviewed.
