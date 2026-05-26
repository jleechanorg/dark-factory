# Amazon Slice: Checkout, Orders, and Reorder

Extend the current Amazon candidate with the checkout and order-management slice from
`benchmarks/amazon-clone/spec.md`.

## Prerequisite

Assume Firestore/auth foundation, catalog/search/reviews, and cart/wishlist/addresses
slices already exist in this workspace. Do not remove or weaken behavior from
prior slices.

## Scope

- Add shopper checkout route and backend flow:
  - `POST /checkout`
  - `POST /checkout/summary`
  - `GET /orders`
  - `GET /orders/:id`
  - `POST /orders/:id/reorder`
- Ensure totals are computed server-side from integer-cent item data and persisted on order.
- Implement inventory decrement, out-of-stock rejection, coupon validation at checkout,
  and cart-to-order atomicity.
- Prevent full card numbers from being logged or stored.
- Clear cart only when order commit succeeds.
- Add order confirmation and order history UI surfaces with deterministic date formatting.
- Add order history/reorder behavior in frontend and backend.
- Persist immutable order snapshots and payment masks.
- Add tests for checkout success plus at least two checkout/order failure paths.

## Validation

Use Antigravity subagents or parallel workers if available:

- one lane for checkout validation and pricing invariants,
- one lane for order persistence and routes,
- one lane for frontend checkout/order views,
- one lane for tests and gate-proof alignment.

## Local proof

Before exiting, run:

```bash
bash benchmarks/amazon-clone/scripts/public_checkout.sh .
```

If it fails, fix the first concrete failure and rerun it.
