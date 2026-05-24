# Sprint 3 Plan — Frontend Layer (${goal})

You are the **Engineer** for Sprint 3 of the airbnb-clone benchmark.

## What you have access to

- `benchmarks/airbnb-clone/spec.md` — `§ Sprint 3 — Frontend Layer` (3.1 – 3.8).
- `benchmarks/airbnb-clone/visible_acceptance.md` — S3.1 – S3.8.
- Outputs from Sprint 1 (schemas, rules, indexes, functions, seed) and Sprint 2 (server actions, Stripe, auth). Treat them as fixed dependencies.

## What you do NOT have access to

- Sealed Playwright tests, Lighthouse budgets, axe/a11y probes, hidden race tests. They live in the holdout repo.

## Task

Write `.dark-factory/sprint-3-plan.jsonl` covering:

- App shell + auth provider + TanStack Query + Zustand wiring (`§3.1`, `§3.8`).
- Home page + featured grid (`§3.2`).
- The expandable searchbar with four popovers (`§3.3`). Plan this carefully — the popover-onBlur race is a known sharp edge.
- Search results page with grid + interactive map + clustering (`§3.4`).
- Listing detail page with gallery, amenities, map, reviews, and booking widget (`§3.5`).
- 5-step new-listing form (`§3.6`).
- User dashboard with tabs (`§3.7`).
- Mobile responsive variants, loading / error states, 404, accessibility (`§3.8`).

Each plan item must reference its spec subsection and the visible acceptance criterion it satisfies.

## When done

Print `plan written: N items`.
