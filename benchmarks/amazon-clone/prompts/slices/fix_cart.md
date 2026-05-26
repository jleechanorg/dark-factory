# Amazon Slice Fix: Cart/Wishlist/Addresses

The cart/wishlist/addresses public gate failed.

Use `benchmarks/amazon-clone/spec.md` as the visible product contract. Do not
read sealed holdouts, hidden scenarios, hidden evaluator source, or sealed
repositories.

## Visible Failure Context

- Last node: `${state._last_node}`
- Last outcome: `${state._last_outcome}`
- Last output:

```text
${state._last_output}
```

## Task

Repair only the cart/wishlist/addresses slice while preserving foundation and
catalog behavior:

- Cart add/update/delete/save-for-later/apply-coupon APIs.
- Wishlist get/add/delete APIs.
- Address CRUD/default APIs.
- Backend totals, validation, persistence, stock checks, and frontend surfaces.
- Tests needed by this slice.

Run:

```bash
bash benchmarks/amazon-clone/scripts/public_cart.sh .
```

Fix the first concrete failure and do not weaken the public gate.
