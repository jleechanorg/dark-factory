# Backend Task

Implement the backend API for `benchmarks/amazon-clone/spec.md`.

## Required Work

- Implement JSON routes listed in the spec.
- Add auth/session behavior for shopper, seller, admin, and guest paths.
- Validate all persisted mutations on the backend.
- Compute cart, coupon, shipping, tax, and checkout totals on the backend.
- Protect checkout inventory updates from overselling.
- Return stable machine-readable error codes.
- Exclude sensitive checkout fields from logs, URLs, storage, and artifacts.
- Add API integration tests against the Firestore emulator.

Do not inspect sealed holdouts or hidden evaluator paths.
