---
description: /auto-factory — drive worldai PRs end-to-end via auto-factory
type: agent-orchestration
execution_mode: one-shot
---
## ⚡ EXECUTION INSTRUCTIONS FOR CLAUDE
**When this command is invoked, YOU (Claude) must execute these steps immediately:**
**This is NOT documentation - these are COMMANDS to execute right now.**

## 🚨 EXECUTION WORKFLOW

Production `/af` execution is Linux-only. Execute this workflow through `/linux`
on `jeff-ubuntu`; do not run local Mac intake, overlay, or tick commands.

### Step 1: Run one tick of the auto-factory skill
Select the repository explicitly. The normal command defaults to worldai; a
caller may export another repository that the selected factory config supports:
```bash
: "${TARGET_REPO:=jleechanorg/worldarchitect.ai}"
export TARGET_REPO
```

Invoke the `auto-factory` skill via the Skill tool:
```
Skill("auto-factory", args="one tick: drive any factory-labeled beads to /green + /er + /code-standards; if no beads, pick up worldai GH issues labeled factory")
```

### Step 2: Verify tick completion
Do not wait on coder subagents inside a tick. Inspect the Linux daemon service,
telemetry, and live worker transcripts, then re-invoke the skill on a later tick
to assess completed work.

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
