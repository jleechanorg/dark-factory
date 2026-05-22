# Kilroy Build Task

Build the Amazon Clone MVP according to the specification.

## Reference Spec

Read the specification you created at: `SPEC.md`

This spec defines the UI layout, data models, API endpoints, and acceptance criteria.

## Your Task

Implement all 10 required user flows according to the spec:

1. **Browse Products** - Home page with grid, category nav
2. **Search Products** - Search bar, autocomplete, results page
3. **View Product Details** - Images, description, reviews, add-to-cart
4. **Shopping Cart** - Add/remove items, quantity, subtotal
5. **User Authentication** - Sign up, sign in, sign out
6. **Checkout Process** - Shipping, payment, confirmation
7. **Order History** - Past orders, details, reorder
8. **Product Reviews** - Submit reviews, view ratings
9. **Wishlist** - Save products, persist across sessions
10. **Admin Inventory** - CRUD products, stock management

## Constraints

### Framework

Choose ONE of:
- Vanilla JavaScript (Node.js backend + vanilla JS frontend)
- React (Next.js or Vite)
- Vue (Nuxt.js or Vite)
- Svelte (SvelteKit or Vite)

Document your choice in Makefile comments.

### Launch Contract

Your implementation MUST satisfy:

```bash
make build    # Install dependencies, compile if needed
make test     # Run test suite (must pass)
make run      # Start server on port 3000
```

### Port Requirement

Server MUST start on port 3000.

### Data

Use realistic sample data:
- At least 20 products across 5 categories
- Sample users for testing
- Sample orders for order history

## Quality Bar

### Must Pass

- [ ] No console errors in browser
- [ ] No PII logging (names, emails, addresses, phones)
- [ ] Responsive on mobile (320px) and desktop (1280px)
- [ ] Accessible (keyboard nav, screen reader friendly)
- [ ] All 10 flows functional end-to-end

### Performance

- [ ] Initial page load under 3 seconds
- [ ] API responses under 500ms
- [ ] Smooth animations (no jank)

### Security

- [ ] No SQL injection vulnerabilities
- [ ] No XSS vulnerabilities
- [ ] Passwords hashed (never stored plaintext)
- [ ] Input validation on all endpoints

## Don't

- Don't add features outside the 10 required flows
- Don't skip error handling
- Don't use placeholder data that breaks flows
- Don't add analytics or tracking
- Don't log sensitive data

## File Structure

Create at minimum:

```
src/
  server.js           # Entry point, port 3000
  routes/             # API route handlers
  models/             # Data models
  middleware/         # Auth middleware, error handling
  public/             # Frontend assets
tests/
  api.test.js         # API endpoint tests
  ui.test.js          # UI interaction tests
Makefile              # build, test, run targets
package.json
```

## Success Criteria

Implementation is complete when:
- `make build` succeeds
- `make test` passes
- `make run` starts server on port 3000
- `curl http://localhost:3000/health` returns 200
- All 10 flows work in browser