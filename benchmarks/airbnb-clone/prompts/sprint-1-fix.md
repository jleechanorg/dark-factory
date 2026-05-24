# Sprint 1 Fix — Data Layer (${goal})

The sealed evaluator reported failures against Sprint 1 outputs.

## Diagnostic surface

The previous pipeline step emits **redacted** failure buckets: scenario names + category (`rules` / `schema` / `cloud-function` / `seed` / `indexes`). That is your **only** signal. Do not search the filesystem for hidden tests, scoring code, or `holdouts/*`.

## What you may change

- `firebase.json`, `.firebaserc`, `firestore.rules`, `storage.rules`, `firestore.indexes.json`.
- `src/lib/schema/**`
- `functions/src/**`
- `scripts/seed.ts`

## What you must NOT change

- `starter/**` (treat as immutable scaffold).
- Any file outside Sprint 1's scope (Sprint 2 server actions, Sprint 3 UI — they don't exist yet).
- The visible spec or the visible acceptance file.

## Hard constraints

- Stay within Sprint 1 scope. If a failure points at logic that belongs to Sprint 2 or Sprint 3, leave it alone and note "out of scope for Sprint 1 fix".
- Do not open or push a pull request.
- Do not search for `holdouts/`, `evaluator/`, or sealed test files.

## When done

State what you changed in one or two sentences and print `sprint-1: fix complete`.
