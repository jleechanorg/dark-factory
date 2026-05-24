# Plan Task

Generate a concrete implementation plan for the Amazon full-stack commerce benchmark.

## Reference Spec

Read the full public specification at `benchmarks/amazon-clone/spec.md`.
Read `benchmarks/amazon-clone/visible_acceptance.md` if it exists.

Treat the public specification as the complete product contract. Do not inspect
sealed evaluator repositories, holdout directories, hidden scenarios, or hidden
test source.

## Required Scope

The final application must include:

- Frontend commerce app with route-backed catalog, detail, cart, checkout,
  account, order history, wishlist, seller, admin, notifications, and
  diagnostics views.
- Backend JSON API that owns validation and persistence.
- Firestore emulator as the local database of record.
- Deterministic seed/reset flow.
- Firestore rules for user, seller, and admin ownership boundaries.
- Validation harness covering lint, unit, API integration, browser checkout,
  Firestore rules probes, and source-size checks.
- At least 5,000 non-generated source lines counted by the validation harness.

## Plan Output

Produce a concise architecture plan with:

- Chosen frontend/backend stack.
- Directory layout.
- Firestore collections and repository boundary.
- API route groups.
- State management approach.
- Seed and reset approach.
- Validation harness approach.
- Risks and sequencing.
