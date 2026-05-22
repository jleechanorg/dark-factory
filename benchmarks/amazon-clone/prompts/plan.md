# Plan Task

Generate a development plan for implementing the Amazon Clone MVP.

## Reference Spec

Read the full specification at: `benchmarks/amazon-clone/spec.md`

The spec defines:
- 10 required user flows (browse, search, cart, checkout, etc.)
- Quality standards (no console errors, responsive, accessible)
- Framework constraints (vanilla JS, React, Vue, or Svelte; any backend language)
- Port requirement: 3000

## Required User Flows

Your plan must address all 10 of the following user flows:

1. **Browse Products** - Home page with product grid, category navigation
2. **Search Products** - Search bar with autocomplete, results page
3. **View Product Details** - Product page with images, description, reviews, add-to-cart
4. **Shopping Cart** - Add/remove items, quantity adjustment, subtotal calculation
5. **User Authentication** - Sign up, sign in, sign out, session management
6. **Checkout Process** - Shipping address, payment method, order review, confirmation
7. **Order History** - View past orders, order details, reorder capability
8. **Product Reviews** - Submit reviews with ratings, view aggregated ratings
9. **Wishlist** - Add/remove products, persist across sessions
10. **Admin Inventory** - Add/edit/delete products, stock management

## Launch Contract

Your plan must ensure the following commands will succeed:

```bash
make build    # Install dependencies, compile/transpile if needed
make test     # Run test suite (must pass)
make run      # Start server on port 3000, must respond to health check
```

## Plan Output

Provide a 2-3 sentence executive summary describing:
- Main files and their purpose
- Overall project structure
- Framework and architecture choices

Then provide a high-level task list (5-10 items) covering:
- Initial project setup
- Core data models
- API endpoints
- Frontend components for each major flow
- Testing strategy
- Deployment configuration