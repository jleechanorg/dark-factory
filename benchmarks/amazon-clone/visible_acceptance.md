# Amazon Clone MVP - Visible Acceptance Criteria

**Version:** 1.1  
**Last Updated:** 2026-06-27  
**Total Points:** 100 (+ 10 bonus)  
**Pass Threshold:** 70/100 points  

**Disclosure model (recommendation A, 2026-06-27 audit):** this file lists the
visible thresholds the implementing agent should target — Lighthouse > 70,
axe-core < 5 violations, concrete viewport/responsive criteria, etc. — as
pass/fail targets. The held-back evaluator adds additional probes beyond these
visible thresholds; the implementing agent cannot enumerate them by reading
this file. This model is shared with `benchmarks/airbnb-clone/visible_acceptance.md`.

---

## Build & Install (10 points)

| # | Criterion | Points | Verification Method |
|---|-----------|--------|---------------------|
| 1.1 | `npm install` completes without errors | 2 | Run install, verify exit code 0 |
| 1.2 | `npm start` or `npm run dev` launches application | 2 | Start server, verify port 3000/5173/8000 listening |
| 1.3 | Initial page load renders without console errors | 2 | Open browser DevTools, verify 0 Error-level logs |
| 1.4 | Product grid displays within 3 seconds of page load | 2 | Measure time from navigation to grid render |
| 1.5 | No broken external resource links (fonts, CDN, images) | 2 | Check Network tab for failed resources |

---

## Core Flows (35 points)

### Product Listing (6 points)

| # | Criterion | Points |
|---|-----------|--------|
| 2.1 | Products display in grid layout | 2 |
| 2.2 | Each product card shows: image, title, price | 2 |
| 2.3 | Star rating displays on product cards | 2 |

### Product Detail (5 points)

| # | Criterion | Points |
|---|-----------|--------|
| 2.4 | Click product navigates to detail page | 2 |
| 2.5 | Detail page shows full product info + quantity selector | 2 |
| 2.6 | Back navigation returns to product listing | 1 |

### Search & Filter (8 points)

| # | Criterion | Points |
|---|-----------|--------|
| 2.7 | Search input filters products by text | 3 |
| 2.8 | Category dropdown filters products | 3 |
| 2.9 | Empty search shows "No products found" message | 2 |

### Cart Management (8 points)

| # | Criterion | Points |
|---|-----------|--------|
| 2.10 | "Add to Cart" adds item to cart | 2 |
| 2.11 | Cart icon shows item count badge | 2 |
| 2.12 | Cart displays items with quantity controls | 2 |
| 2.13 | Remove item removes from cart | 2 |

### Checkout & Confirmation (8 points)

| # | Criterion | Points |
|---|-----------|--------|
| 2.14 | Checkout form renders with all required fields | 2 |
| 2.15 | Form validation shows errors for invalid fields | 3 |
| 2.16 | Successful checkout shows order confirmation with order ID | 3 |

---

## Persistence (15 points)

| # | Criterion | Points | Verification Method |
|---|-----------|--------|---------------------|
| 3.1 | Cart state persists after page refresh | 5 | Add item, refresh page, verify item still in cart |
| 3.2 | Cart quantity changes persist | 5 | Change quantity, refresh, verify quantity preserved |
| 3.3 | Order clears cart on successful checkout | 5 | Complete order, verify cart is empty |

---

## UI/UX (20 points)

| # | Criterion | Points | Verification Method |
|---|-----------|--------|---------------------|
| 4.1 | Responsive grid: 3-4 columns on desktop | 5 | View at 1440px, count columns |
| 4.2 | Responsive grid: 2 columns on tablet | 5 | View at 768px-1024px, count columns |
| 4.3 | Responsive grid: 1 column on mobile | 5 | View at 375px width, count columns |
| 4.4 | No horizontal scroll at any viewport | 5 | Scroll horizontally at each breakpoint, verify no scrollbar |

---

## Accessibility (10 points)

| # | Criterion | Points | Verification Method |
|---|-----------|--------|---------------------|
| 5.1 | All images have alt text | 3 | Inspect images, verify alt attributes present |
| 5.2 | Form inputs have associated labels | 3 | Inspect form, verify explicit label elements |
| 5.3 | Focus states visible on interactive elements | 2 | Tab through page, verify focus indicators |
| 5.4 | Keyboard navigation works (Tab, Enter, Escape) | 2 | Navigate entirely via keyboard |

---

## Security (10 points)

| # | Criterion | Points | Verification Method |
|---|-----------|--------|---------------------|
| 6.1 | No email addresses in console.log | 3 | Search source for console.log with email pattern |
| 6.2 | Card numbers masked in console (****1234) | 4 | Check console after entering card number |
| 6.3 | Full card number not displayed on confirmation | 3 | Inspect confirmation page, verify masked format |

---

## Evidence (10 points bonus)

| # | Criterion | Points | Verification Method |
|---|-----------|--------|---------------------|
| 7.1 | Screencast video of full checkout flow | 5 | Video file present, covers add-to-cart through confirmation |
| 7.2 | Accessibility audit report (axe-core or similar) | 3 | Report shows < 5 violations |
| 7.3 | Performance audit (Lighthouse score > 70) | 2 | Lighthouse Performance score documented |

---

## Scoring Summary

| Section | Max Points |
|---------|-----------|
| Build & Install | 10 |
| Core Flows | 35 |
| Persistence | 15 |
| UI/UX | 20 |
| Accessibility | 10 |
| Security | 10 |
| **Total** | **100** |
| Bonus: Evidence | +10 |

**Pass Threshold:** 70/100 points  
**With Bonus:** 75/110 points

---

## Test Protocol

### Automated Checks
1. Run `npm install && npm start`
2. Open browser at localhost
3. Verify page loads without errors
4. Run axe-core accessibility audit
5. Run Lighthouse performance audit
6. Record screencast of full flow

### Manual Verification
1. Execute each core flow manually
2. Test responsive breakpoints using DevTools
3. Test keyboard navigation
4. Inspect console for PII leakage

### Scoring
- Each criterion is binary (met / not met)
- Partial credit not awarded
- Zero-tolerance on security criteria (PII leakage = auto-fail)