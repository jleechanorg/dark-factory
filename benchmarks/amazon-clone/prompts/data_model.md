# Data Model Task

Implement the Firestore data model and persistence foundation for the Amazon
full-stack benchmark.

## Required Work

- Define all collections from `benchmarks/amazon-clone/spec.md`.
- Create repository modules for products, inventory, carts, wishlists,
  addresses, orders, reviews, coupons, notifications, moderation events,
  seller profiles, and metrics snapshots.
- Use deterministic IDs for seed fixtures.
- Store money as integer cents.
- Keep checkout-sensitive payment fields out of Firestore.
- Add tests for repository read/write behavior using the Firestore emulator.

Do not inspect sealed holdouts or hidden evaluator paths.
