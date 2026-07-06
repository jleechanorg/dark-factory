---
description: /auto-factory — drive worldai PRs end-to-end via auto-factory
type: agent-orchestration
execution_mode: one-shot
---
## ⚡ EXECUTION INSTRUCTIONS FOR CLAUDE
**When this command is invoked, YOU (Claude) must execute these steps immediately:**
**This is NOT documentation - these are COMMANDS to execute right now.**

## 🚨 EXECUTION WORKFLOW

### Step 1: Run one tick of the auto-factory skill
Invoke the `auto-factory` skill via the Skill tool:
```
Skill("auto-factory", args="one tick: drive any factory-labeled beads to /green + /er + /code-standards; if no beads, pick up worldai GH issues labeled factory")
```

### Step 2: Verify tick completion
Wait for any spawned coder subagents to complete (they run in background). Then re-invoke the skill to run the verifier tick.

### Step 3: Repeat until all beads reach READY
Re-invoke the skill until `$H list QUEUED` and `$H list DISPATCHED` are empty AND all ATTESTED beads have all-green gate assessment.

### Step 4: Report status
Report:
- Beads picked up (count + ids)
- PRs driven to merge-ready state (count + PR numbers)
- Beads still pending (count + ids + reasons)
- Recommended next action if any

## 📋 REFERENCE

See `.claude/skills/auto-factory/SKILL.md` for the full protocol.