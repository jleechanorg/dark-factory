# Benchmark — all-nodes-coverage

A self-contained Dark Factory benchmark that **deterministically exercises
every declared graph node in a single run**: `start → plan → implement →
verify (fail) → fix → verify (pass) → exit`.

Most pipelines either skip `fix` (when the first implementation passes
verify) or never reach `exit` (when verify never recovers and the loop
exhausts via `max_visits`). This benchmark engineers a guaranteed first-pass
failure + recoverable fix using a **hidden test contract** that the visible
spec does not enumerate — the test suite asserts a module-level
`__version__` constant the spec does not mention.

## What gets tested

- **Pipeline parser** — DOT loads, `verify` carries `validation="true"`
- **Conditional edges** — both `verify → exit [outcome=success]` and
  `verify → fix [outcome!=success]` fire in the same run
- **`max_visits` loop guard** — `fix` is capped at 3 visits (not reached here
  on the happy path)
- **`tool` node `cwd` + `${state.X}` substitution** — `cwd` resolves to the
  AO worker's worktree path stashed in state
- **Engine `validation="true"` opt-in** — without this opt-in, the `tool`
  verify node would not clear `_unresolved_failure`, and `exit` would
  report `failure` even after a successful retry
- **AO backend session reuse** — `plan` spawns a session; `implement`,
  `fix` reuse it via `ao send`; per-codergen state is preserved on the
  worker side

## Files

| Path | Role |
|---|---|
| `pipeline.dot` | 6-node graph (start, plan, implement, verify, fix, exit) |
| `specs/roman.md` | Visible feature spec (also inlined into `prompts/plan.md`) |
| `prompts/plan.md` | Inlines the spec; asks for a plan, no code |
| `prompts/implement.md` | Asks for the impl in `df_demo3/roman.py` |
| `prompts/fix.md` | Diagnoses via `pytest -v`, instructs worker to read assertion message |
| `_holdout/test_roman.py` | **Sealed test suite** — the worker never sees this during plan/implement |
| `_holdout/__init__.py` | Empty; pytest package marker |

## How to run

```bash
cd ~/projects/dark-factory

# Best path: use AO + Sonnet (Anthropic OAuth), real LLM
SRC=$HOME/.hermes/agent-orchestrator.yaml
DST=$HOME/.dark-factory/temp-configs/sonnet-mctrl-test.yaml
cp "$SRC" "$DST"
yq -i '.projects."mctrl-test".modelByCli."claude-code".model = "claude-sonnet-4-6"' "$DST"

AO_CONFIG_PATH="$DST" .venv/bin/python -m runner \
  --pipeline benchmarks/all-nodes-coverage/pipeline.dot \
  --workdir /Users/jleechan/projects/mctrl_test \
  --goal "Implement to_roman per the inlined spec" \
  --backend ao \
  --ao-project mctrl-test \
  --ao-agent claude-code \
  --cxdb ~/.dark-factory/cxdb-benchmark.sqlite
```

Or echo backend (deterministic dry-run, no LLM):

```bash
.venv/bin/python -m runner \
  --pipeline benchmarks/all-nodes-coverage/pipeline.dot \
  --workdir /tmp/echo-target \
  --goal "echo probe" \
  --backend echo
```

The echo run will exhaust the fix loop (echo doesn't write files), proving
the conditional edges and `max_visits` cap fire correctly.

## Success criteria

After a real-backend run, the CXDB step transcript should match:

```
0  start        success
1  plan         success
2  implement    success
3  verify       failure       <- forced by hidden __version__ check
4  fix          success
5  verify       success       <- pytest 21/21
6  exit         success       <- thanks to validation="true" on verify
```

Coverage report (run via `runner.parser` + sqlite query):

```
declared: ['exit', 'fix', 'implement', 'plan', 'start', 'verify']
hit (6/6): ['exit', 'fix', 'implement', 'plan', 'start', 'verify']
miss     : []
```

## Why this is a good benchmark

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
