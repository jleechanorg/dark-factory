# Firestore Rules Task

Implement local Firestore emulator security rules for the Amazon benchmark.

## Required Work

- Add rules for shopper-owned users, carts, wishlists, addresses, orders,
  reviews, and notifications.
- Add seller ownership rules for seller profiles, products, and inventory.
- Add admin-only access for moderation actions and suspended content controls.
- Protect cross-user reads and writes.
- Protect cross-seller product mutations.
- Add rule probe tests that run against the emulator.

Do not inspect sealed holdouts or hidden evaluator paths.
