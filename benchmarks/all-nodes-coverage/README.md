# Benchmark - all-nodes-coverage

A Dark Factory benchmark for the all-nodes pipeline shape:
`start -> plan -> implement -> verify -> fix -> verify -> exit`.

The visible repo contains only the pipeline, prompts, and public feature spec.
The verification contract lives in the sibling sealed holdouts repo and is
reached through the `holdout_eval` node. The spawned coding worker gets only
redacted evaluator output, not test source or expected hidden values.

## What gets tested

- **Pipeline parser** - DOT loads, and `verify` carries `validation="true"`.
- **Conditional edges** - `verify -> exit [outcome=success]` and
  `verify -> fix [outcome!=success]` route from sealed evaluator verdicts.
- **`max_visits` loop guard** - `fix` is capped at 3 visits.
- **`holdout_eval` implementation substitution** - `implementation` resolves
  to the AO worker worktree path stored in state.
- **Engine `validation="true"` opt-in** - a successful verify clears the
  unresolved failure before `exit`.
- **AO backend session reuse** - `plan` spawns a session; `implement` and
  `fix` reuse it via `ao send`.

## Files

| Path | Role |
|---|---|
| `pipeline.dot` | 6-node graph (start, plan, implement, verify, fix, exit) |
| `specs/roman.md` | Visible feature spec (also inlined into `prompts/plan.md`) |
| `prompts/plan.md` | Inlines the spec; asks for a plan, no code |
| `prompts/implement.md` | Asks for the implementation in `df_demo3/roman.py` |
| `prompts/fix.md` | Uses redacted sealed-evaluator feedback to request a narrow fix |
| sibling holdouts repo | Sealed evaluator and scenarios; not stored in this benchmark tree |

## How to run

```bash
cd ~/projects/dark-factory

export DARK_FACTORY_HOLDOUTS=<sealed-holdouts-repo>

AO_CONFIG_PATH="$HOME/.dark-factory/temp-configs/sonnet-mctrl-test.yaml" \
  .venv/bin/python -m runner \
  --pipeline benchmarks/all-nodes-coverage/pipeline.dot \
  --workdir /Users/jleechan/projects/mctrl_test \
  --goal "Implement to_roman per the inlined spec" \
  --backend ao \
  --ao-project mctrl-test \
  --ao-agent claude-code \
  --cxdb ~/.dark-factory/cxdb-benchmark.sqlite
```

Echo backend dry-run:

```bash
export DARK_FACTORY_HOLDOUTS=<sealed-holdouts-repo>

.venv/bin/python -m runner \
  --pipeline benchmarks/all-nodes-coverage/pipeline.dot \
  --workdir /tmp/echo-target \
  --goal "echo probe" \
  --backend echo
```

The echo run should exhaust the fix loop because echo does not write files.
That proves conditional routing and the `max_visits` cap without claiming
feature success.

## Success Criteria

When a real-backend run recovers from an initial evaluator failure, the CXDB
step transcript has this shape:

```text
0  start        success
1  plan         success
2  implement    success
3  verify       failure
4  fix          success
5  verify       success
6  exit         success
```

When the sealed evaluator returns only redacted feedback that is insufficient
for recovery, a run may correctly exhaust at `fix` instead. That still proves
sealed isolation; it does not prove full all-node recovery.

Coverage report:

```text
declared: ['exit', 'fix', 'implement', 'plan', 'start', 'verify']
hit (6/6): ['exit', 'fix', 'implement', 'plan', 'start', 'verify']
miss     : []
```

## Why This Benchmark

- **Cheap**: ~2 minutes wall, no PR is opened, files live in a throwaway
  AO worktree
- **Deterministic forcing function**: the hidden `__version__` test always
  fails on the first impl; Sonnet always adds it on fix turn 1
- **Exercises every load-bearing edge**: both branches of the verify
  conditional, the `fix → verify` loop edge, and the `validation="true"`
  unresolved-failure clearing
- **Conforms to the Attractor pattern**: separate spec source, sealed
  evaluator (the worker sees neither the test path nor its content during
  plan/implement turns; the spec is inlined into the prompt so the worker
  doesn't need filesystem access for it)

## Caveat — isolation strength

Per [`CLAUDE.md`](../../CLAUDE.md), the rule is: the *spawned coding agent*
(this benchmark's AO worker) must not read holdouts/tests. The *operator*
running the benchmark obviously can — they wrote them.

In this benchmark the worker isolation is **prompt-discipline + verify-time
injection**, not `sandbox-exec`-enforced:

- The plan/implement prompts inline the visible spec and never mention
  `_holdout/`. Until the first `verify` runs, the worker's worktree
  contains no test source.
- `sandbox-exec` only blocks reads of `~/projects/dark-factory-holdouts/`,
  not arbitrary `_holdout/` directories elsewhere.
- Once `verify` runs (it `cp`s the fixtures into the worktree so pytest
  can find them), the test source is on disk inside the worktree. The
  `fix.md` prompt instructs the worker to read pytest's assertion
  messages, not the test source — but a worker that ignores the prompt
  could `cat _holdout/test_roman.py` directly and trivially pass.

For the **stricter, AttractorBench-grade** isolation (training-contamination
prevention + leak-proof against an adversarial worker) use the
`holdout_eval` node type instead of `tool`: that runs the sealed evaluator
at `$DARK_FACTORY_HOLDOUTS/evaluator/run.py`, which the worker can't read
(sandbox deny rule + stripped env), and returns only a verdict.

This benchmark deliberately chose the lighter `tool`-based design because
its purpose is to **exercise every graph node in a single deterministic
run**, not to defend against an adversarial worker — Sonnet under our
prompts doesn't cheat, and the forcing function is reliable. If you want
to exercise the same six nodes against a sealed evaluator, swap the
`verify` node's type from `tool` (with `validation="true"`) to
`holdout_eval` and move the test source to `dark-factory-holdouts/`.

### Sealed `roman` scenario — where it lives

The sealed-evaluator equivalent of `_holdout/test_roman.py` is committed in
the sibling repo at `$DARK_FACTORY_HOLDOUTS/holdouts/roman/scenarios.yaml`
(canonical path: `~/projects/dark-factory-holdouts/holdouts/roman/`). It
covers the same 19 conversion cases (1, 4, 9, 40, 94, 900, 1994, 3999, …)
plus the hidden `df_demo3.roman.__version__ == "1.0.0"` module contract.
The scenarios.yaml is the source of truth — do **not** mirror it back into
this benchmark, by design. To swap this benchmark onto the sealed
evaluator, replace the `verify` node with:

```
verify [type="holdout_eval",
        feature="roman",
        implementation="${state.ao.worktree}",
        validation="true"]
```

and drop the `cp -R …/_holdout` step; the evaluator's sandbox enforces both
the conversion correctness and the hidden metadata contract without the
worker ever seeing the test source.
