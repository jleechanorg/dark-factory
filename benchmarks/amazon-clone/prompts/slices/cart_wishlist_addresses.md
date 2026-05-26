# Amazon Slice: Cart, Wishlist, Addresses

Extend the current Amazon candidate with the cart/wishlist/addresses slice from
`benchmarks/amazon-clone/spec.md`.

## Prerequisite

Assume the Firestore/auth foundation and catalog/search/reviews slices already
exist in this workspace. Do not remove or weaken foundation or catalog
behavior, public routes, Make targets, seed/reset, diagnostics, auth/session,
source-size validation, products, search, or reviews.

## Scope

Implement:

- Root and `/api` cart endpoints:
  - `GET /cart`
  - `POST /cart/items`
  - `PATCH /cart/items/:productId`
  - `DELETE /cart/items/:productId`
  - `POST /cart/save-for-later`
  - `POST /cart/apply-coupon`
- Cart totals computed by backend using integer cents, including subtotal,
  discounts, shipping estimate, tax estimate, and grand total.
- Quantity validation, stock limit checks, invalid product rejection, and item
  persistence across requests/session.
- Root and `/api` wishlist endpoints:
  - `GET /wishlist`
  - `POST /wishlist/items`
  - `DELETE /wishlist/items/:productId`
- Root and `/api` address endpoints:
  - `GET /addresses`
  - `POST /addresses`
  - `PATCH /addresses/:id`
  - `DELETE /addresses/:id`
  - `POST /addresses/:id/default`
- Frontend cart drawer, full cart route, quantity controls, save-for-later,
  coupon application, wishlist view, wishlist add/remove, address book, and
  default address controls.
- Tests for cart, wishlist, and address behavior.

## Execution

Use Antigravity subagents or parallel workers if available:

- one lane for cart backend/domain logic,
- one lane for wishlist/address backend APIs,
- one lane for frontend cart/wishlist/address views,
- one lane for tests and public gate repair.

Integrate all outputs into one runnable workspace.

## Local proof

Before exiting, run:

```bash
bash benchmarks/amazon-clone/scripts/public_cart.sh .
```

If it fails, fix the first concrete failure and rerun it.
