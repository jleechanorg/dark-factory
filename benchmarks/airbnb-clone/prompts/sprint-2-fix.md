# Sprint 2 Fix — Backend Layer (${goal})

The sealed evaluator reported failures against Sprint 2.

## Diagnostic surface

Redacted failure buckets from the previous step (`auth` / `validation` / `server-action` / `stripe` / `search` / `rate-limit`). No other signal.

## What you may change

- `src/lib/firebase/**`, `src/lib/schema/**`, `src/lib/search/**`, `src/lib/result.ts`, `src/lib/rate-limit.ts`.
- `src/app/actions/**`, `src/app/api/webhooks/stripe/**`.

## What you must NOT change

- `starter/**`, Sprint 1 outputs (rules / schemas / functions / indexes / seed), or Sprint 3 UI.

## Hard constraints

- Stripe stays in test mode.
- Server action envelope shape is non-negotiable: `{ ok: true, data } | { ok: false, error: { code, message } }`.
- Do not open or push a PR; do not search for sealed paths.

## When done

State the change in one or two sentences and print `sprint-2: fix complete`.
