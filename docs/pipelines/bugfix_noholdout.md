# `pipelines/slim/bugfix_noholdout.dot`

## When to use this pipeline

Use `bugfix_noholdout.dot` when iterating an **in-flight bug-fix branch** that
has no sealed behavioral holdout — i.e. the feature was not added with a
matching `holdouts/<feature>/scenarios.json` in the sealed
`$DARK_FACTORY_HOLDOUTS` repo. The lane runs the full `implement → test →
fresh-eyes review → /es → /er` chain but **omits the `holdout_eval` node on
purpose**, because there is nothing sealed to evaluate.

## When NOT to use this pipeline

- The bug fix has a sealed behavioral holdout — use `pipelines/bug_fix.dot`
  (the canonical red/green + holdout + adversarial-review loop with `max 3
  fix` visits). Reach for `bug_fix.dot` whenever the feature has
  `holdouts/<feature>/scenarios.json` and you want the red/green discipline.
- The diff is a feature add (not a bug fix) — use
  `pipelines/slim/minimal_feature.dot` (greenfield feature, with holdout) or
  `pipelines/slim/minimal_pr.dot` (in-flight PR iteration, with holdout).
- The code is already on the branch and only needs validation (no fix loop)
  — use `pipelines/slim/redgreen_claudeaf.dot` (single `test` node + `/es` +
  `/er`; no `implement`/`fix`).
- You only need a SHA-bound adversarial review of an existing diff with no
  implementation work — use `pipelines/slim/review_pr.dot` (one `codergen`
  review + one `gate_er`).

## The trade-off — no sealed holdout

Every implement-bearing lane normally runs the sealed behavioral holdouts
(see `docs/pipeline-selection.md` "Holdout-always policy").
`bugfix_noholdout.dot` is a **deliberate, narrow exception**: the only
behavioral gate it can run is the cross-vendor adversarial reviewer
(`gate_er`, resolved through the priority queue
`codex > minimax > agy` with `prefer_adversarial="true"`
so the run-level coder backend is excluded). `gate_es` (evidence standards)
precedes it. The lane is not for "I forgot to write a holdout" — it is for
branches where the underlying feature predates the holdout program and
adding one is out of scope for this fix.

**Merge confidence bar = `gate_er` verdict alone.** There is no
`holdout_eval` fallback. A green `/er` is the merge signal; a non-pass
verdict routes back to the bounded `fix` loop (`max_visits="3"`) until
exhaustion, then the run terminates as a failure.

## 1-line example invocation

```bash
dark-factory --pipeline pipelines/slim/bugfix_noholdout.dot \
  --goal "fix the in-flight X regression" --backend claude \
  --state slim.test_command='.venv/bin/python -m pytest tests/test_x.py -q' \
  --cxdb ~/.dark-factory/cxdb.sqlite
```
