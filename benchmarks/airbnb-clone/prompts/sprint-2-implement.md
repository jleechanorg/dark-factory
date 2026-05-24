# Sprint 2 Implement — Backend Layer (${goal})

Implement Sprint 2 from `.dark-factory/sprint-2-plan.jsonl`. Work only from `spec.md §ports 2.x` and `visible_acceptance.md §S2.x`.

## What to build

- `src/lib/firebase/admin.ts` and `src/lib/firebase/client.ts` with emulator auto-detection.
- Email/Google/GitHub auth wired against the Auth emulator.
- Zod schemas in `src/lib/schema/` for listing / booking / review / search-filters.
- Server actions under `src/app/actions/**` for every operation in `spec.md §2.4`.
- Stripe integration: PaymentIntent creation inside the booking server action; `src/app/api/webhooks/stripe/route.ts` with signature verification; refund flow.
- A search text tokenizer + cursor pagination helper in `src/lib/search/`.
- Result envelope helper (`Result<T>`) used by every server action.
- A Firestore-backed token-bucket rate limiter applied to high-volume actions.

## Hard constraints

- Every server action must return the `{ ok, data | error }` envelope.
- Auth is required for every write; `auth.uid` is verified against the operation's owner.
- Stripe uses **test mode** keys only; never `live_`.
- Do not modify `starter/`, do not touch UI files (Sprint 3).
- Do not open or push a pull request.
- Do not search for `holdouts/`, `evaluator/`, sealed test files.

## Verification before finishing

```bash
pnpm test:server-actions
```

Fix obvious failures within this step. Leave larger failures for the dedicated `fix` node.

## When done

Print `sprint-2: implemented`.
