# Implement Task

Implement the Amazon full-stack commerce benchmark according to
`benchmarks/amazon-clone/spec.md`.

## Non-Negotiable Requirements

- Build a real frontend, a real backend API, and Firestore emulator persistence.
- Implement all 20 public user stories in the spec.
- Use Firestore emulator as the local database of record for persisted commerce
  state.
- Provide deterministic seed/reset data.
- Provide Firestore rules for shopper, seller, admin, and evaluator boundaries.
- Provide a validation harness that proves the app is not a shallow static demo.
- Meet the 5,000 non-generated source-line floor.
- Do not inspect sealed holdouts or hidden evaluator paths.

## Launch Contract

The implementation must support these commands or documented equivalents:

```bash
make build
make seed
make run
make test
make validate-size
```

`make run` must start the frontend, backend, and Firestore emulator together.
Startup output must print the frontend URL, backend URL, backend health URL,
emulator UI URL, and diagnostics URL.

The visible public acceptance gate also checks:

- `firebase.json`, `firestore.rules`, and Firebase/Firestore dependencies.
- `make seed` and `make validate-size`.
- At least 5,000 non-generated source lines, including at least 2,000 frontend
  lines under `src/public/`.
- Runtime HTTP smoke for `/health`, `/api/products`, diagnostics, and adding a
  product to the cart.
- Visible coverage for wishlist, seller, admin, notifications, checkout, order
  history, diagnostics, and Firestore emulator configuration.

## Implementation Guidance

Prefer a boring, maintainable architecture:

- Frontend components and route views separated from API client code.
- Backend route handlers separated from validation, domain logic, and Firestore
  repositories.
- Money represented as integer cents internally.
- Checkout totals computed on the backend.
- Checkout inventory writes protected from overselling.
- Sensitive checkout fields excluded from logs, storage, URLs, and artifacts.
- Tests that reset local data deterministically.

Complete the product, not only the happy path.

## Parallelization

This is a broad implementation task. If the active backend supports
subagents or parallel workers, use them to split frontend, backend, Firestore,
test harness, seed data, and documentation work. You remain responsible for
integrating the outputs into one runnable full-stack app before exiting.
