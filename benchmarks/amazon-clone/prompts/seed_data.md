# Seed And Reset Task

Implement deterministic local seed/reset behavior for the Amazon full-stack
benchmark.

## Required Work

- Seed at least 60 products across at least 8 departments.
- Seed shoppers, sellers, an admin, addresses, carts, wishlists, coupons,
  orders, reviews, notifications, moderation events, inventory records, seller
  profiles, and metrics snapshots.
- Make seed/reset idempotent.
- Print collection counts after seed completes.
- Ensure test setup can reset data between tests.
- Keep exact hidden evaluator IDs unknown; use clear deterministic public
  fixture IDs for ordinary tests and demos.

Do not inspect sealed holdouts or hidden evaluator paths.
