# Airbnb Clone Benchmark

Attractor-style **full-stack short-term-rental marketplace** benchmark. The implementing agent builds the product against a visible spec; a sealed evaluator scores 90 tasks against hidden contracts.

Inspired by AgentLoop's [Airbnb clone case study](https://www.agentloop.run/blog/airbnb-clone-case-study), but with **Firestore (local emulator only)** in place of Supabase end-to-end. No Postgres, no live cloud project, no live Stripe.

This is the largest benchmark in the repo:
- 90 tasks across 3 sprints (Data → Backend → Frontend)
- Targets ~36 hours of agent wall-time
- Forces multi-LLM-budget coordination (auth + storage + payments + maps + real-time availability)

## Files

| Path | Audience | What |
|---|---|---|
| `DESIGN.md` | Operator | Full design: tech stack swap, 90-task enumeration, scoring, holdout layout, references. |
| `spec.md` | Implementing agent | Feature-level acceptance per sprint. No adversarial probes. |
| `visible_acceptance.md` | Implementing agent | Happy-path self-checks. Passing all is **necessary, not sufficient**. |
| `prompts/sprint-{1,2,3}-{plan,implement,fix}.md` | Implementing agent | Per-node prompt templates (use `${goal}` substitution). |
| `pipelines/sprint-{1,2,3}-*.dot` | DOT runner | Each sprint as a graph. |
| `pipelines/airbnb-clone.dot` | DOT runner | Master: sprint-1 → holdout-eval → sprint-2 → … → exit. |
| `starter/` | Implementing agent | Pre-installed Next.js 14 + Tailwind + Shadcn + Firebase emulator config. |
| `scripts/` | Operator | Smoke + matrix scripts. |
| `docs/` | Operator | Per-run findings, comparisons. |

## Revealed vs. sealed

### Revealed to the implementing agent

- `spec.md`, `visible_acceptance.md`
- `starter/` scaffold
- `prompts/`
- `pipelines/*.dot`
- Redacted evaluator output (aggregate verdicts + failure categories only)

### Sealed (in `$DARK_FACTORY_HOLDOUTS/holdouts/airbnb-clone/`)

- `scenarios.yaml` — 90 scoring scenarios
- `tests/rules/` — Firestore Security Rules probes (positive + negative + adversarial leak)
- `tests/server-actions/` — admin-SDK integration tests
- `tests/e2e/` — Playwright against `next dev` + emulator
- `tests/lighthouse/` — perf + a11y budgets
- `tests/adversarial/` — race conditions (availability double-book), Rules leak attempts, OWASP top-10 probes
- `evaluator/score_airbnb.py` — driver that produces the per-scenario verdict JSON

The implementing agent must never see the sealed tree. The operator (you) may.

## Quick start

### Smoke (no LLM, no Firebase)

```bash
python -m runner \
  --pipeline benchmarks/airbnb-clone/pipelines/sprint-1-data.dot \
  --goal "smoke airbnb sprint 1" \
  --backend echo
```

### Real sprint run via AO

```bash
export DARK_FACTORY_HOLDOUTS=~/projects/dark-factory-holdouts

python -m runner \
  --pipeline benchmarks/airbnb-clone/pipelines/airbnb-clone.dot \
  --goal "Build the airbnb clone end-to-end per spec.md" \
  --backend ao \
  --ao-project airbnb-clone \
  --ao-agent claude-code \
  --feature airbnb-clone \
  --cxdb ~/.dark-factory/cxdb.sqlite \
  --evidence-bundle /tmp/airbnb-clone-run-$(date +%Y%m%d-%H%M%S)
```

Use the `ao-model-override` skill (`spawn-with-model.sh`) if you want to pin a specific Sonnet/Opus build for the workers.

### Visualise a pipeline

```bash
dot -Tpng benchmarks/airbnb-clone/pipelines/airbnb-clone.dot -o /tmp/airbnb.png
```

## Scoring

Per-task verdict: `pass` / `partial` / `fail` / `error`. Each task contributes up to 2.5 points:

- 1.0 functional (visible happy path)
- 1.0 hidden contract (adversarial / edge case)
- 0.5 perf / a11y / security

Max: 90 × 2.5 = **225 points**. Aggregate metrics recorded in CXDB metadata and the evidence bundle:

- `total_score / 225`
- `tasks_passed / 90`
- `total_tokens`, `total_cost_usd`, `total_wall_ms`
- `lighthouse_perf`, `lighthouse_a11y`, `axe_violations`
- `rules_leak_probes_blocked / total`
- `e2e_pass_rate`

See `DESIGN.md §7` for the full rubric.

## Comparison axes vs AgentLoop

| Axis | AgentLoop reported | Target here |
|---|---|---|
| Prompts → tasks amplification | 3 → 87 | 3 → ≥ 90 |
| Wall time | not disclosed | < 36 h |
| Cost | not disclosed | < $20 |
| Tasks completed before human intervention | not disclosed | ≥ 90% |

## Status

- 2026-05-24 — `DESIGN.md`, `spec.md`, `visible_acceptance.md` landed.
- Next — sealed scenarios and first real run.
