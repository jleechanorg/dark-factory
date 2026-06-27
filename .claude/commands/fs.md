---
description: "/fs — generate main + attractor specs with codex cold review. DEFAULT: invoke the real dark-factory binary against spec_gen or a binary-owned dynamic spec graph, then echo proof metadata."
type: quality
execution_mode: immediate
aliases: [fs]
---

# /fs — Spec Generation (binary-first)

`/fs` produces **two** reviewed specs:
- `spec.md` — the main spec (acceptance criteria, test command, non-goals,
  lane-independence matrix)
- `attractor_spec.md` — the attractor spec (convergence target, observable
  convergence criteria, anti-attractor states)

**Default invocation path** is the real `dark-factory` binary, normally
against `pipelines/slim/spec_gen.dot`. The binary may use Claude/workflow
logic internally to build or select a dynamic spec graph, but the run is
unproven unless `/fs` shows the literal binary command, run metadata, logs,
and evidence envelope.

The default graph must preserve these nodes or their generated equivalents:
main spec plan, independent cold review, bounded main-spec fix loop,
attractor-spec plan, independent cold review, bounded attractor fix loop, and
exit.

**Usage**:

```
/fs <spec description>                  # DEFAULT: binary spec_gen run
/fs --pipeline spec_gen <description>   # explicit static binary pipeline
/fs --dynamic-graph <description>       # binary-owned dynamic spec graph
/fs --skip-attractor <description>      # main spec only, still binary-first
/fs --review <spec_path>                # binary-backed review of an existing main spec
/fs --review-attractor <path>           # binary-backed review of an existing attractor spec
```

## Action

1. Parse `$ARGUMENTS`.
2. If `--review` or `--review-attractor` is present, construct a binary-backed
   review command. Use `benchmarks/attractor-spec-review/pipelines/review_slim.dot`
   or another saved DOT graph selected by the binary; do not perform a
   prose-only review and call it a factory run.
3. Otherwise, construct and run the binary spec-generation command below.
4. For read-only graph reference output, use `/factory-spec --show`.

```bash
export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
export DARK_FACTORY_HOLDOUTS="${DARK_FACTORY_HOLDOUTS:-$HOME/projects/dark-factory-holdouts}"
export PATH="$HOME/.local/bin:$PATH"
cd <target repo>
dark-factory \
  --pipeline pipelines/slim/spec_gen.dot \
  --goal "<echo of $ARGUMENTS>" \
  --backend <echo for smoke | claude/codex/agy/minimax for real> \
  --feature <feature> \
  --cxdb ~/.dark-factory/cxdb.sqlite
```

### Step 0: detect context

Classify greenfield vs brownfield so the binary run can frame the spec goal
appropriately:

- **Greenfield** (new feature, no existing code in the target repo): the
  spec is the source of truth; the binary graph writes/reviews `spec.md`.
- **Brownfield** (existing code, replacement/delete work): the spec must
  enumerate what stays, what goes, and the net-LOC constraint per
  `factory-spec` delete-first rules.

This is informational; the binary graph or generated DOT should surface both
possibilities to the spec author.

### Default graph: what gets produced

The default `pipelines/slim/spec_gen.dot` path produces or updates:

- `spec.md`
- `attractor_spec.md`
- cold-review outputs for both specs
- fix-loop handoff when either review fails

If the binary uses a dynamic generated spec graph instead, the generated DOT
must be saved or echoed and must preserve the required default nodes.

**Pass criterion:** `final_outcome == "success"` for the binary run, with both
main and attractor reviews passing.

### Graph reference

Read `.claude/skills/factory-spec/SKILL.md` for full mode details. The
default static path is `pipelines/slim/spec_gen.dot`:

```
start → explore_in → explore_fanout → {explore_concept, explore_auth,
       explore_reuse, explore_risks} → explore_join → explore_stitch →
       explore_out
     → plan_main        [plan.md → spec.md]
     → review_main      [codex cold review of spec.md]              ─┐
     → plan_attractor   [plan_attractor.md → attractor_spec.md]     │ if main review fails → fix_main → review_main
     → review_attractor [codex cold review of attractor_spec.md]    ─┐
     → exit                                                            │ if attractor review fails → fix_attractor → review_attractor
```

## Honesty rules

- Quote the **actual** `dark-factory` command run.
- If the binary run fails, **surface the trace** and stop. Do not assume
  "we can fix the spec later" without feeding the full review output into the
  next fix loop.
- If `--backend echo` was used, label the run as a wiring smoke, not a
  real validation. Echo-mode review verdicts are not real LLM verdicts.
- Do not claim an in-Claude workflow or `Skill()` result is a factory run.
  The only valid proof is an actual `dark-factory` binary invocation plus the
  required proof block from that run.

End every `/fs` response with this proof block. Missing any required line means
the run is unproven:

```bash
# Literal command run:
cd /Users/jleechan/projects/<target-repo>
DARK_FACTORY_HOME=~/projects/dark-factory \
DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts \
PATH="$HOME/.local/bin:$PATH" \
dark-factory \
  --pipeline pipelines/slim/spec_gen.dot \
  --goal "<echo of $ARGUMENTS>" \
  --backend <backend> \
  --feature <feature> \
  --cxdb ~/.dark-factory/cxdb.sqlite
# Run ID: <id>
# CXDB SHA: <sha>
# Final outcome: <success|failure|exhausted|error>
# Exit code: <integer>
# Wall-clock: <duration>
# Logs: <path>
# Evidence envelope: <path>
```

## Why binary-first is the default

Same reasoning as `/f`: the user's 2026-06-27 clarification requires default
binary invocation. Claude/workflow logic may build a dynamic DOT graph behind
the binary, but the durable proof remains the DOT graph, CXDB, logs, and
evidence envelope.

Use static `pipelines/slim/spec_gen.dot` unless a binary-owned dynamic graph is
needed and saved/echoed.

## See also

- `/factory-spec` — graph reference and read-only spec tooling
- `/f` — full Dark Factory loop; also binary-first by default
- `$DARK_FACTORY_HOME/.claude/workflows/dark-factory.md` — optional source
  material for binary-owned dynamic graph generation; not proof by itself
