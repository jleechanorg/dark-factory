# Amazon Slice: Firestore/Auth Foundation

Implement the foundation slice of `benchmarks/amazon-clone/spec.md`.

## Scope

Build only the shared foundation needed by all later Amazon full-stack slices:

- Firestore emulator configuration with `firebase.json` and `firestore.rules`.
- Firestore-backed local persistence layer. It may use a deterministic local
  adapter for tests, but the app must expose and document
  `FIRESTORE_EMULATOR_HOST`.
- Deterministic seed/reset data for users, roles, products, inventory, carts,
  wishlists, addresses, notifications, seller profiles, moderation events, and
  metrics snapshots.
- Root backend API endpoints from the public spec:
  - `GET /health`
  - `GET /metrics`
  - `POST /seed/reset`
  - `POST /auth/register`
  - `POST /auth/login`
  - `POST /auth/logout`
  - `GET /session`
  - `GET /diagnostics`
- `/api/...` aliases for the same endpoints.
- `make build`, `make seed`, `make run`, `make test`, and
  `make validate-size`.
- A source-size validator that enforces at least 5,000 non-generated source
  lines and at least 2,000 frontend source lines.
- Startup output from `make run` that prints frontend URL, backend URL, backend
  health URL, emulator UI URL, and diagnostics URL.

## Non-goals for this slice

Do not attempt to complete catalog, cart, checkout, seller/admin, or browser
evidence in this slice except for seed data and route placeholders needed by
the foundation.

## Execution

Use Antigravity subagents or parallel workers if available:

- one lane for Firestore/data/rules,
- one lane for auth/session/API routing,
- one lane for Makefile/runtime/validation,
- one lane for tests.

Integrate all outputs into one runnable workspace.

## Local proof

Before exiting, run:

```bash
bash benchmarks/amazon-clone/scripts/public_foundation.sh .
```

If it fails, fix the first concrete failure and rerun it.
