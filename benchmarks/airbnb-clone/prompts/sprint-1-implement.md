# Sprint 1 Implement — Data Layer (${goal})

Implement Sprint 1 of the airbnb-clone benchmark following `.dark-factory/sprint-1-plan.jsonl`.

## Source of truth

- `benchmarks/airbnb-clone/spec.md` — work only from `§ Sprint 1 — Data Layer`.
- `benchmarks/airbnb-clone/visible_acceptance.md` — when you finish, the S1.1 – S1.5 self-checks must pass locally.

## What to build (this step)

Walk the plan file `.dark-factory/sprint-1-plan.jsonl` in dependency order and produce all of the following:

- `firebase.json`, `.firebaserc`, `firestore.rules`, `storage.rules`, `firestore.indexes.json`.
- TypeScript Zod schemas under `src/lib/schema/` for every collection in `spec.md §1.1`.
- Cloud Functions under `functions/src/` implementing the four triggers in `spec.md §1.4`.
- Composite indexes per `spec.md §1.3`.
- `scripts/seed.ts` per `spec.md §1.5` — deterministic; re-running yields identical IDs.

## Hard constraints

- Do **not** modify anything in `starter/` outside the files listed in your plan.
- Do **not** install Supabase, Prisma, Postgres, or any cloud-only Firebase SDK helpers.
- Use the Firebase Admin SDK (`firebase-admin`) for seed and Cloud Functions; use the Client SDK (`firebase`) only when called out by Sprint 2.
- Every emulator should auto-detect via `FIRESTORE_EMULATOR_HOST` / `FIREBASE_AUTH_EMULATOR_HOST` / `FIREBASE_STORAGE_EMULATOR_HOST` / `FUNCTIONS_EMULATOR=true`.
- Do **not** open or push a pull request.
- Do **not** search the filesystem for `holdouts`, `evaluator`, or sealed test files. Those are sealed.

## Verification you must run before finishing

```bash
# Step 1 — compile Cloud Functions (required before emulator can load them)
cd functions && npm install --silent && npm run build && cd ..

# Step 2 — start emulators, seed, run rules tests
firebase emulators:exec --only firestore,auth,storage,functions \
  "npx ts-node scripts/seed.ts && echo seed:ok" \
  --project airbnb-clone-dev
```

If either command exits non-zero, fix the failure before reporting done. Do not consume more than one fix loop in this step; the dedicated `fix` node will pick up remaining failures.

## When done

Print `sprint-1: implemented`. Do not open or push a PR.
