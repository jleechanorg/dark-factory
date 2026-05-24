# Implement Task

Implement the Amazon Clone MVP according to the specification.

## Reference Spec

Read the full specification at: `benchmarks/amazon-clone/spec.md`
Read the public acceptance criteria at: `benchmarks/amazon-clone/visible_acceptance.md`

Together, those files define the complete visible product contract. Do not add
flows that are not present there, and do not inspect hidden evaluator details.

## Launch Contract

Your implementation MUST satisfy the following commands:

```bash
make build    # Install dependencies, compile/transpile if needed
make test     # Run test suite (must pass)
make run      # Start server on port 3000, respond to health check
```

The server MUST:
- Start on port 3000
- Respond to `GET /health` with a 200 status code
- Serve all static assets correctly

## Implementation Requirements

### Files to Create

You must create the following structure (minimum viable implementation):

```
/benchmarks/amazon-clone/
  src/
    server.js          # Express/HTTP server, port 3000
    routes/
      products.js      # Product API endpoints
      cart.js          # Cart API endpoints
      orders.js        # Order API endpoints
    models/
      product.js       # Product data model
      cart.js          # Cart data model
      order.js         # Order data model
    public/
      index.html       # Main HTML entry
      app.js           # Frontend JavaScript
      styles.css       # Styles
  tests/
    products.test.js   # Product API tests
    cart.test.js        # Cart API tests
    orders.test.js      # Order API tests
  package.json
  Makefile
  SPEC.md              # Copy of spec.md for reference
```

### Quality Standards

All visible flows in `spec.md` and `visible_acceptance.md` must work.

### Do

- Make the application actually work end-to-end
- Ensure `make build` installs all dependencies
- Ensure `make test` runs and passes
- Ensure `make run` starts the server on port 3000
- Handle edge cases (empty cart, invalid input, etc.)
- Use realistic sample data for products
- Implement proper error handling

### Don't

- Don't log PII (names, emails, addresses, phone numbers)
- Don't add features outside the 10 required flows
- Don't use placeholder data that breaks flows
- Don't skip error handling
- Don't use console.log for debugging in final code
- Don't add analytics or tracking pixels

## Framework Choice

You may use any of:
- Vanilla JavaScript (Node.js server + vanilla JS frontend)
- React (create-react-app or Vite)
- Vue (Vue CLI or Vite)
- Svelte (SvelteKit or Vite)
- Next.js

If using a framework, ensure all build commands are in Makefile.
