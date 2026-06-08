---
name: dark-factory
description: "Run the Dark Factory DOT pipeline runner against a goal. Slash command: /factory. Implements StrongDM's Attractor pattern as an external Python runner — .dot files are the versioned artifact, sealed holdouts live in a separate repo, every step is recorded to CXDB, and the Healer clusters failures into diagnoses. Use when you want the goal_harness idea executed as a reproducible external pipeline instead of in-Claude subagent dispatch."
---

# /factory — Dark Factory Pipeline Runner

## Purpose

Run a goal through the Dark Factory `.dot` pipeline runner at
`~/projects/dark-factory/`. The runner is the Python implementation of
StrongDM's Attractor pattern (cf. brynary/attractor, strongdm/attractor).

Versus `/h` / `/goal_harness`:

| Aspect | `/h` (goal_harness) | `/factory` (this skill) |
|--------|---------------------|-------------------------|
| Engine | Claude session dispatches subagents | **`dark-factory` binary** (uv install) |
| Artifact | Skill prose + tool calls | `.dot` graphviz file |
| Recording | Conversation transcript | SQLite CXDB row-per-step |
| Holdouts | Adversarial subagent reads diff | Sealed repo agent never sees |
| Reuse | Tied to this session | Replayable from `.dot` alone |
| Cost | High (multi-agent context) | Low (one CLI per node) |

Use `/factory` when you want the goal_harness idea as a reproducible external
pipeline; use `/h` when you want an interactive in-session loop.

## Repos

- `~/projects/dark-factory/` — the runner (factory code; agent can see this)
- `~/projects/dark-factory-holdouts/` — sealed scenarios (agent must NEVER read)
- Pushed: `https://github.com/jleechanorg/dark-factory` + `dark-factory-holdouts`

## Available pipelines

| `.dot` file | Flow | When to use |
|-------------|------|-------------|
| `pipelines/factory/hello.dot` | plan → implement → holdout → fix-loop → exit | Add a new feature with a holdout scenario |
| `pipelines/factory/gates.dot` | start → holdout → /es → /er → /code_standards → exit | Validate an already-implemented diff (Attractor-style 4-gate harness as `.dot`) |
| `pipelines/factory/pr_gates.dot` | start → /es → /er → /code_standards → exit | Validate an already-implemented in-flight PR diff (bypasses holdouts) |
| `pipelines/slim/minimal_feature.dot` | explore → plan → implement → test → review → holdout → gates → exit | Full production pipeline from scratch with a holdout |
| `pipelines/slim/minimal_pr.dot` | explore → plan → implement → test → review → gates → exit | Slim in-flight PR iteration loop with parameterized tests (bypasses holdouts) |

You can also write your own `.dot` and pass it via `--pipeline`.

## How to invoke this skill

The user types `/factory $ARGUMENTS`. Parse the arguments and run the steps
below. **Do not default to a single pipeline** — select the graph that matches
the task (see **Pipeline selection** below) unless the user passed `--pipeline`.

### Step 0 — Select pipeline (mandatory when `--pipeline` omitted)

1. Read the goal and apply **factory-spec Step 0** (greenfield vs brownfield).
2. Choose a pipeline from
   [docs/pipeline-selection.md](../../../docs/pipeline-selection.md) (or the table in
   **Available pipelines** below).
3. **Tell the user** which pipeline you chose and why before running.
4. If brownfield replace/delete: encode delete-first rules in the goal; do not
   use a greenfield additive pipeline by default.

If `$ARGUMENTS` already contains `--pipeline`, skip auto-selection.

### Short-name expansion

When `--pipeline` is a short name (no `/` or `.dot`), expand under
`$DARK_FACTORY_HOME`:

| Short name | Path |
|------------|------|
| `gates` | `pipelines/factory/gates.dot` |
| `hello` | `pipelines/factory/hello.dot` |
| `pr_gates` | `pipelines/factory/pr_gates.dot` |
| `minimal_feature` | `pipelines/slim/minimal_feature.dot` |
| `minimal_pr` | `pipelines/slim/minimal_pr.dot` |
| `review_slim` | `benchmarks/attractor-spec-review/pipelines/review_slim.dot` |
| `review_full` | `benchmarks/attractor-spec-review/pipelines/review_full.dot` |

### Arg parsing

Honor these flags inside `$ARGUMENTS`:

- `--pipeline <name>` — short name (`gates`, `hello`, `pr_gates`, `minimal_pr`,
  `minimal_feature`, `review_slim`, `review_full`) or path to a `.dot`. If
  omitted, **auto-select from the goal** (Step 0 above) — never blindly default
  to `gates.dot` or `minimal_feature.dot`.
- `--feature <name>` — holdout feature (required when the pipeline includes a
  `holdout_eval` node; default `hello`)
- `--backend echo|claude|codex|ao|agy` — default `claude`. Use `echo` for cost-free
  dry-runs that verify wiring without LLM calls.
- `--max-steps <N>` — default 100
- `--cxdb <path>` — default `~/.dark-factory/cxdb.sqlite` (shared across runs
  so the Healer can cluster across history)
- `--state KEY=VALUE` — pre-seed context variables; repeatable. Extremely useful for setting parameterized test commands (e.g. `--state slim.test_command="pytest tests/..."`) during PR iteration loops.

Whatever is left after flag parsing is the **goal description**.

### Steps

1. **Verify binary install**. The factory runs via the **`dark-factory` binary**
   (not `python -m runner` from source). Check:
   ```bash
   export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
   export PATH="$HOME/.local/bin:$PATH"
   command -v dark-factory && dark-factory --help 2>/dev/null || true
   test -x "$DARK_FACTORY_HOME/bin/dark-factory" || {
     echo "ERROR: run $DARK_FACTORY_HOME/install.sh first"
     exit 1
   }
   ```
   If missing, tell the user to clone
   `https://github.com/jleechanorg/dark-factory` and run `./install.sh`, then stop.

2. **Environment**:
   ```bash
   export DARK_FACTORY_HOME="${DARK_FACTORY_HOME:-$HOME/projects/dark-factory}"
   export DARK_FACTORY_HOLDOUTS="${DARK_FACTORY_HOLDOUTS:-$HOME/projects/dark-factory-holdouts}"
   export PATH="$HOME/.local/bin:$PATH"
   ```

3. **Run the pipeline** from the **target repo** (implementation workdir = cwd).
   Pipelines and prompts resolve from `$DARK_FACTORY_HOME`; code changes land in cwd:
   ```bash
   cd "<TARGET_REPO>"   # e.g. the product repo being built
   dark-factory \
     --pipeline pipelines/<PATH_TO_DOT>.dot \
     --goal "<GOAL>" \
     --backend <BACKEND> \
     --feature <FEATURE> \
     --cxdb <CXDB_PATH> \
     --state <KEY>=<VALUE>
   ```

4. **Surface the verdict**. The last line of stdout is a JSON summary with
   `final_outcome`, `pipeline`, `goal`, `steps`, `trace`. Report:
   - Pipeline name + final outcome
   - Per-node trace (one line each: `node  outcome  preview`)
   - Exit code of the runner

5. **Run the Healer** (only if `final_outcome != "success"`):
   ```bash
   df-healer --cxdb <CXDB_PATH>
   ```
   Render the resulting markdown table inline. Each row is a failure cluster
   with a prescription template the user can act on.

6. **Sealed holdouts reminder**. If the run included a `holdout_eval` node,
   remind the user that you (and any subagent) did NOT see the scenarios at
   `~/projects/dark-factory-holdouts/holdouts/<feature>/` — only the
   PASS/FAIL verdict per scenario name leaked back.

## Output contract

End every `/factory` invocation with:

```
Pipeline:       <name>
Final outcome:  PASS | FAIL | exhausted | stuck
Steps:          <n>
CXDB:           <path>   (run `df-healer --cxdb <path>` to re-cluster)
```

Plus the trace and, on failure, the Healer report.

## Adding a new feature

To add a new sealed-holdout feature `foo`:

1. Write `~/projects/dark-factory/specs/foo.md` (agent-visible).
2. Write `~/projects/dark-factory/prompts/foo/{plan,implement,fix}.md`.
3. Write `~/projects/dark-factory-holdouts/holdouts/foo/scenarios.yaml`
   in the sealed repo (the implementing agent must never read this).
4. Either reuse `pipelines/factory/hello.dot` (which is feature-agnostic) by
   passing `--feature foo`, or write `pipelines/factory/foo.dot` with custom
   nodes.

## Honesty rules

- Always tell the user the **actual command** you ran (`dark-factory ...` or `df-healer ...`). No paraphrasing.
- Always quote the **runner's exit code** verbatim.
- Never claim a holdout passed if you read the scenarios — that breaks the
  adversarial guarantee. If you read `~/projects/dark-factory-holdouts/`,
  declare the run **invalid** and rerun the pipeline.
- If `--backend echo` was used, label the run as a wiring smoke, not a real
  validation.

## Known limits

- The `claude` backend shells out to `claude --print
  --dangerously-skip-permissions` — each node is a fresh non-interactive
  session. Long agentic loops should use `/h` instead.
- `_parse_verdict` falls back to scanning the tail for a standalone PASS/FAIL
  token; if the gate command emits a debug line with just "PASS" the runner
  will trust it. For high-stakes gates require explicit `VERDICT:` markers.
- CXDB is per-process — don't share one across threads.
