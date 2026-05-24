# Sprint 2 Plan — Backend Layer (${goal})

You are the **Engineer** for Sprint 2 of the airbnb-clone benchmark.

## What you have access to

- `benchmarks/airbnb-clone/spec.md` — read `§ Sprint 2 — Backend Layer` (sections 2.1 – 2.7).
- `benchmarks/airbnb-clone/visible_acceptance.md` — S2.1 – S2.6 self-checks.
- Everything Sprint 1 produced (schemas, rules, functions, seed). Treat the Sprint 1 surface as fixed; if it is wrong, file the issue in your plan but don't modify it from this step.

## What you do NOT have access to

- Hidden adversarial tests, sealed evaluator source, `holdouts/*`.
- Live Firebase or Stripe credentials. Stripe is **test mode only**.

## Task

Write `.dark-factory/sprint-2-plan.jsonl`, one record per item, covering:

- Admin + client SDK initialisation (`spec.md §2.1`).
- Auth providers (`§2.2`).
- Zod validation schemas (`§2.3`).
- Every server action listed in `§2.4`.
- Stripe PaymentIntent + webhook + refund (`§2.5`).
- Search tokenizer and cursor pagination (`§2.6`).
- Envelope + rate limiting (`§2.7`).

Each item must be implementable in one focused step; cite the spec subsection it covers.

## When done

Print `plan written: N items`. Do not write production code in this step.
