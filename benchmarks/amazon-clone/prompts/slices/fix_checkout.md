# Amazon Slice Fix: Checkout/Orders

The checkout/orders public gate failed.

Use `benchmarks/amazon-clone/spec.md` as the visible contract. Do not read sealed holdouts,
hidden scenarios, hidden evaluator source, or sealed repositories.

## Visible Failure Context

- Last node: `${state._last_node}`
- Last outcome: `${state._last_outcome}`
- Last output:

```text
${state._last_output}
```

## Task

Repair only checkout and order-management behaviors while preserving all prior slice work:

- `POST /checkout`
- `POST /checkout/summary`
- `GET /orders`
- `GET /orders/:id`
- `POST /orders/:id/reorder`
- Checkout totals, inventory checks, coupon application, order atomics, and cart clearing.
- Reorder and confirmation flows.
- Tests for success plus at least two failure cases.

Run:

```bash
bash benchmarks/amazon-clone/scripts/public_checkout.sh .
```

Fix the first concrete failure and do not weaken the public gate.
