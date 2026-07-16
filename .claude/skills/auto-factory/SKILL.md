---
name: auto-factory
description: Use when the user explicitly invokes /af or /auto-factory for a coding task.
---

# Auto-Factory (`/af`)

Use this skill only when the user explicitly invokes `/af` or `/auto-factory`.
Once invoked, all coding LLM work for that task remains inside `/af`; interactive
sessions must not bypass it with inline coding or direct worker dispatch. Requests
that do not invoke `/af` follow the normal coding workflow.

## Target contract

1. Resolve the exact target repository from the supplied issue/PR URL or the
   current repository. Never default to `worldarchitect.ai`, and never match a
   same-number issue or PR in another repository.
2. For an existing PR, add or preserve its `factory` label in that repository.
   For a new goal, create or update a factory-labeled GitHub issue or local bead
   in the target repository.
3. Verify adoption in daemon telemetry and verify any spawned worker metadata
   names the exact repository, issue/PR, requested runtime, and agent.
4. If the factory cannot route that repository, record a precise routing
   blocker against the target artifact and report `BLOCKED`. Do not fall back
   to inline coding or direct AO/Codex/Claude dispatch outside `/af`.

## Drive loop

- Run or wake the repository-aware factory intake, then monitor rather than
  replacing it with an interactive coding session.
- Treat implementation agents, AO workers, Codex, Claude, and reviewers as
  factory internals. Preserve requested worker/runtime parameters exactly.
- Continue through implementation, tests, review remediation, and exact-head
  evidence until the PR is `READY` or merged. Respect repository-specific merge
  authorization; when merge is not authorized, stop at verified `READY`.
- A daemon that is running is not proof of adoption. Require target-specific
  telemetry, worker metadata, or PR activity before claiming progress.

## Monitoring

Use the platform supervisor (`launchctl` on macOS, `systemctl --user` on Linux),
the daemon JSONL telemetry under `~/Library/Logs/dark-factory/`, and the exact
worker session output. Report the target artifact URL, factory state, current
head SHA, gates, and any actionable blocker.
