# Activity — 2026-06-27 — Factory-runs-the-factory contract (addendum)

## Context

This is an **addendum** to the 2026-06-27 compliance review activity log. Same date, same session — different request.

## Operator request

> "lets also make bead/roadmap to make sure /fs actually runs the real /f pipeline in an adversarial spec producer mode and after running /fs or /f the command to run must be shown to prove we ran the factory program vs letting claude freestyle it"

Follow-up clarification in the same thread:

> "I want to default to the real binary but that binary can use a claude workflow to build dynamic dot graph but always have default nodes"
>
> "not just fix nodes, all nodes should receive full output"
>
> "the shadow codex mode should be default true, but ensure its a param and we can keep it off later"
>
> "also some of these reviewer nodes may be redundant and can be parallelzied, make a bead and do this in the first round too"

## What this means

The current `/f` and `/fs` slash commands default to invoking a **Claude Workflow** (in-Claude subagent dispatch via `Skill("dark-factory", args="...")`). The `--legacy` flag falls through to the actual `dark-factory --pipeline ... --goal ... --backend ... --cxdb ...` binary call.

Operator's concern:
1. **Binary-first dynamic graph mode** — `/fs` should run the real `dark-factory` binary. The binary may use Claude/workflow logic internally to build or select a dynamic DOT graph, but the externally proven default path must be the binary and the graph must include required default nodes: plan/spec producer, independent cold review, fix loop, attractor/spec review, independent cold review, fix loop, exit.
2. **Evidence of binary invocation** — after `/f` or `/fs`, the actual command line must be **echoed** in the reply to prove the dark-factory binary was invoked. No claim of "I ran the factory" without the literal `dark-factory --pipeline ... --goal ... --backend ... --cxdb ...` (or equivalent) command shown in the reply.
3. **Full free-form handoff** — every node, not only fix nodes, must pass full free-form output forward to downstream LLM nodes. Capped fields are allowed only for previews and indexes, never as the source of truth for LLM handoff.
4. **Shadow Codex default-on** — reviewer/gate paths should run `codex exec --yolo` in parallel by default and log both the primary and shadow review outputs. This must remain parameterized so operators can disable it later.
5. **Reviewer dedupe and parallelization** — redundant serial reviewers should be audited and, when independent, parallelized without losing separate reviewer outputs or the combined coder handoff.

## Why this matters

This is the verification-before-completion principle (`superpowers:verification-before-completion`, `~/.claude/skills/evidence-standards.md`) applied to the factory itself:
- The factory-evolve review found that the factory cannot review itself because reviewers live inside the same exhausted fix-loop.
- An in-Claude workflow dispatch can produce output that *looks* like a factory run but is actually freestyle subagent work.
- The only way to prove the dark-factory program ran is to show the binary invocation.

## Beads created

| Bead | Title | Priority |
|------|-------|----------|
| `jleechan-92g` | [A1] /fs default to real dark-factory binary in adversarial spec-producer mode (not in-Claude workflow) | P1 |
| `jleechan-ion` | [A2] Mandatory command-line echo after /f and /fs (no freestyle claim without binary invocation proof) | P1 |
| `jleechan-0a6` | [A3] Audit /f and /fs current default path: ensure factory-runs-the-factory contract (binary or workflow calls binary, not freestyle) | P2 |
| `jleechan-tql` | [A4] Pipeline-level evidence envelope: every dark-factory run produces a .jsonl + command echo + CXDB SHA pair saved to evidence/<run-id>/ | P2 |
| `jleechan-fla` | [A5] All nodes pass full free-form output forward; only preview fields may be capped | P1 |
| `jleechan-wlz` | [A6] Shadow Codex reviewer mode defaults on and remains opt-out by parameter | P1 |
| `jleechan-j9w` | [A7] Detailed factory logs capture every node input, prompt, full output, and LLM/tool transcript | P1 |
| `jleechan-xx5` | [A8] Dedupe and parallelize reviewer nodes while preserving independent outputs | P1 |

## First-round execution order

Work in batches of five beads, with a direct `codex exec --yolo` cold review after each bead is implemented and before moving to the next bead. The first batch is:

1. `jleechan-0a6` — audit and pin the current `/f`/`/fs` default path.
2. `jleechan-ion` — mandatory command echo in `/f`/`/fs`/`/factory` output.
3. `jleechan-tql` — run evidence envelope under `evidence/<run-id>/`.
4. `jleechan-fla` — full free-form handoff for every node, previews capped separately.
5. `jleechan-xx5` — reviewer dedupe/parallelization for independent reviewer nodes.

`jleechan-wlz` and `jleechan-j9w` are P1 and may be pulled into the first batch if the audit shows they are already partly implemented or are prerequisite for proving A2/A4/A5.

## Required changes (operator-visible)

### `~/.claude/commands/fs.md` — default to legacy

Either:
- Remove the Claude Workflow default and run `dark-factory --pipeline pipelines/slim/spec_gen.dot --goal "<spec description>" --backend <echo|claude|codex|agy|minimax> --feature <feature> --cxdb ~/.dark-factory/cxdb.sqlite` directly.
- Or keep the workflow as a wrapper, but force the workflow to shell out to the `dark-factory` binary for every spec_gen phase (not synthesize specs in-Claude).

### `~/.claude/commands/f.md` — same treatment

Force the workflow to invoke `dark-factory` binary for each phase, not freestyle.

### Evidence contract

After every `/f` or `/fs` invocation, the reply MUST contain a code block of the form:

```bash
# Literal command run:
cd /Users/jleechan/projects/<target-repo>
DARK_FACTORY_HOME=~/projects/dark-factory \
DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts \
PATH="$HOME/.local/bin:$PATH" \
dark-factory \
  --pipeline pipelines/slim/spec_gen.dot \
  --goal "<echo of $ARGUMENTS>" \
  --backend claude \
  --feature <feature> \
  --cxdb ~/.dark-factory/cxdb.sqlite
# Run ID: <id>
# CXDB SHA: <sha>
# Final outcome: <success|failure|exhausted>
# Wall-clock: <duration>
```

The "Run ID" / "CXDB SHA" / "Final outcome" lines are emitted by `dark-factory` itself; if any are missing, the run did not happen — surface that.

## Cross-references

- Compliance review: `docs/factory-evolve-research/review-2026-06-27.md` (gap #1: factory non-converging on `fix`)
- `/f` skill: `~/.claude/commands/f.md`
- `/fs` skill: `~/.claude/commands/fs.md`
- spec_gen pipeline: `pipelines/slim/spec_gen.dot` (already has `prefer_adversarial="true"` on both reviewers)
- Memory: `feedback_2026-06-22_user_pivot_default_nodes_over_custom.md` (the original workflow-default pivot)
- Skills: `superpowers:verification-before-completion`, `~/.claude/skills/evidence-standards.md`
