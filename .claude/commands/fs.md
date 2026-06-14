---
description: "/fs — alias for /factory-spec — generate main + attractor specs, codex cold-reviews both"
type: quality
execution_mode: immediate
aliases: [fs]
---

# /fs — Alias for /factory-spec (Two-Phase Spec Generation)

Pure alias: execute `/factory-spec` with `$ARGUMENTS`.

`/fs` produces **two** reviewed specs by default:
- `spec.md` — the main spec (implementation path: acceptance criteria,
  test command, non-goals, lane-independence matrix)
- `attractor_spec.md` — the attractor spec (convergence target,
  observable convergence criteria, anti-attractor states, verification
  command, consistency-with-main-spec)

A cold adversarial codex reviewer signs off on **both** before exit. The
attractor spec is generated after the main spec passes review, so the
attractor spec can reference the codex-signed-off `spec.md` for the
consistency check.

Pass `--skip-attractor` to produce only the main spec.

Single writer for the workflow is
`.claude/commands/factory-spec.md` (modes + install details in
`.claude/skills/factory-spec/SKILL.md`). Do not duplicate workflow steps
in this file.

Equivalent to: `/factory-spec $ARGUMENTS`
