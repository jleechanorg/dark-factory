# Amazon Clone MVP - Behavioral Holdout Scenarios

This document references the 10 behavioral holdout scenarios used to evaluate an implementing agent's Amazon Clone MVP implementation.

## Overview

The holdout scenarios live in the **sibling holdouts repository** (`~/projects/dark-factory-holdouts`), not in this repo. This separation ensures:

1. The implementing agent cannot read the test specifications
2. Evaluation is sealed and adversarial
3. Results reflect real conformance, not test-awareness

### Sealed Evaluation Contract

The evaluator (`$DARK_FACTORY_HOLDOUTS/evaluator/run.py`) runs against the implementing agent's artifact (running server at `http://localhost:3000`). The evaluator has access to:

- The holdout scenario definitions
- Playwright automation scripts
- The public spec (`benchmarks/amazon-clone/spec.md`)

The implementing agent only has access to:

- The spec (`benchmarks/amazon-clone/spec.md`)
- Prompt templates (`benchmarks/amazon-clone/prompts/`)
- The pipeline graph (`benchmarks/amazon-clone/pipelines/`)

## The 10 Behavioral Scenarios

### 1. product_listing_loads

**Description:** Product listing page loads with 5 or more products displayed in the product grid.

**Evaluation Method:**
- Launch browser at http://localhost:3000
- Wait for `#product-grid` selector to be visible
- Count product cards within the grid
- Assert count >= 5

**Points Value:** 3.5 points

---

### 2. search_filters_products

**Description:** Search functionality filters products by text match against product titles.

**Evaluation Method:**
- Navigate to product listing page
- Enter a search query (e.g., "wireless")
- Verify grid content changes to only matching products
- Confirm non-matching products are hidden

**Points Value:** 3.5 points

---

### 3. detail_page_has_price_title_description

**Description:** Product detail page displays all required fields: price, title, and description.

**Evaluation Method:**
- Click on a product from the listing
- Verify `#product-title` exists and is non-empty
- Verify `#product-price` exists and is non-empty
- Verify `#product-description` exists and is non-empty

**Points Value:** 3.5 points

---

### 4. cart_add_remove_quantity

**Description:** Cart operations (add, remove, change quantity) work correctly.

**Evaluation Method:**
- Add a product to cart from detail page
- Verify cart count increments
- Increase quantity to 3
- Verify quantity reflects 3
- Remove item from cart
- Verify cart count decrements and cart is empty

**Points Value:** 3.5 points

---

### 5. checkout_rejects_invalid_email

**Description:** Checkout form validates email and rejects invalid formats.

**Evaluation Method:**
- Add item to cart and proceed to checkout
- Enter invalid email formats:
  - Missing @ symbol: `testuser`
  - Missing TLD: `test@user`
  - No domain: `test@`
- Verify form submission fails with validation error
- Verify no checkout completion

**Points Value:** 3.5 points

---

### 6. checkout_completes_valid_order

**Description:** Valid checkout with complete form submission creates order confirmation.

**Evaluation Method:**
- Add item to cart
- Proceed to checkout with valid form data:
  - Valid email (e.g., `test@example.com`)
  - Shipping address
  - Payment simulation (test card or mock)
- Submit form
- Verify order confirmation page appears
- Verify confirmation contains order identifier

**Points Value:** 3.5 points

---

### 7. cart_persists_refresh

**Description:** Cart state persists across page reload using localStorage.

**Evaluation Method:**
- Add items to cart
- Refresh the page (navigate to same URL)
- Verify cart still contains added items
- Verify cart count reflects items added before refresh

**Points Value:** 3.5 points

---

### 8. mobile_view_no_horizontal_overflow

**Description:** Mobile viewport (380px width) displays without horizontal scroll.

**Evaluation Method:**
- Set viewport to 380px width
- Navigate to product listing page
- Scroll vertically
- Assert no horizontal scrollbar appears
- Assert all content fits within 380px width

**Points Value:** 3.5 points

---

### 9. basic_a11y_labels

**Description:** Accessibility labels are present on interactive and media elements.

**Evaluation Method:**
- Scan page for:
  - All `<img>` elements have non-empty `alt` attributes
  - All `<input>` elements have associated `<label>` elements
  - Cart icon has `aria-label` or `aria-labelledby`
  - Buttons have accessible names
- Assert all checks pass

**Points Value:** 3.5 points

---

### 10. no_pii_or_card_logged

**Description:** Console logs do not contain PII (emails) or credit card numbers.

**Evaluation Method:**
- Enable console log capture via Playwright
- Perform checkout flow with test email and card
- Scan captured console output for:
  - Email patterns (anything matching `*@*.*`)
  - Credit card patterns (16-digit sequences)
  - Any logged form input containing personal data
- Assert no PII or card numbers appear in logs

**Points Value:** 3.5 points

---

## Launch Contract

The implementing agent must produce an artifact that satisfies this launch contract:

```bash
make build && make test && make run
```

### Startup Requirements

1. **`make build`** - Compiles/transpiles the application without errors
2. **`make test`** - Runs self-tests and returns exit code 0
3. **`make run`** - Starts server on port 3000 and stays running
4. **Evaluator connects** to http://localhost:3000

### Server Expectations

- Must listen on port 3000
- Must serve the Amazon Clone application
- Must be responsive within 10 seconds
- Must maintain session (cart state in localStorage)

---

## Evaluation Order

The evaluator runs scenarios in this order:

1. **Build passes** - `make build` exits with code 0
2. **Self-tests pass** - `make test` exits with code 0
3. **Server starts** - `make run` spawns server on port 3000
4. **Playwright scenarios** - Each scenario runs against localhost:3000
5. **PII scan** - Console logs are scanned for sensitive data
6. **Score computed** - Results aggregated and reported

### Scoring Summary

| Category | Points | Details |
|----------|--------|---------|
| Holdout Scenarios (10) | 35 | 3.5 points each |
| Edge Cases | 15 | Additional validation scenarios |
| **Total** | **50** | Maximum achievable |

Note: Holdout scenarios account for 70% of the evaluation score (35/50 points).

---

## Implementation Guidance

The implementing agent should:

1. Read `spec.md` for feature requirements
2. Implement core functionality matching the spec
3. Ensure localStorage cart persistence works
4. Add basic accessibility attributes during implementation
5. Avoid logging form input to console
6. Test viewport responsiveness for 380px width

The agent should **not**:
- Attempt to read holdout scenario definitions
- Hardcode test assertions for expected behaviors
- Log user input to console for debugging

---

## Repository Structure

```
~/projects/dark-factory-holdouts/
└── amazon-clone/
    ├── evaluator/           # Sealed evaluation logic
    │   └── run.py          # Main evaluator entry point
    ├── scenarios/          # Playwright scenario definitions
    │   ├── product_listing_loads.py
    │   ├── search_filters_products.py
    │   └── ... (8 more scenarios)
    └── edge_cases/         # Additional validation scenarios
```

This structure ensures complete isolation between the spec that guides implementation and the tests that verify conformance.