---
description: /af — alias for /auto-factory
type: agent-orchestration
execution_mode: one-shot
---
## ⚡ EXECUTION INSTRUCTIONS FOR CLAUDE
**When this command is invoked, YOU (Claude) must execute these steps immediately:**

This is an alias for `/auto-factory`. Invoke the auto-factory skill:
```
Skill("auto-factory", args="one tick: drive any factory-labeled beads to /green + /er; /code-standards and /zfc are advisory checks (not required; if present and failing, still blocks all_green); if no beads, pick up worldai GH issues labeled factory")
```

Then verify, repeat until beads reach READY, and report status.

See `.claude/skills/auto-factory/SKILL.md` for the full protocol.