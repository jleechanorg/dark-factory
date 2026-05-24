# Sprint 3 Fix — Frontend Layer (${goal})

The sealed evaluator reported failures against Sprint 3.

## Diagnostic surface

Redacted failure buckets (`searchbar` / `search-results` / `listing-detail` / `new-listing-form` / `dashboard` / `mobile` / `a11y` / `loading-states`). No other signal.

## What you may change

- `src/app/**` (UI pages, layouts, loading.tsx, error.tsx, not-found.tsx).
- `src/components/**` (Shadcn-derived primitives and feature components).
- `src/store/**` (Zustand slices).
- `src/lib/hooks/**`.

## What you must NOT change

- Server actions, Cloud Functions, schemas, rules, seed (Sprints 1 + 2).
- `starter/**` scaffolding.

## Hard constraints

- No silent fallbacks: errors render the route's `error.tsx`.
- Keyboard navigation and focus traps must remain intact.
- Searchbar must continue to keep popovers open while the user is interacting with them.
- Do not open or push a PR; do not search for sealed paths.

## When done

State the change in one or two sentences and print `sprint-3: fix complete`.
