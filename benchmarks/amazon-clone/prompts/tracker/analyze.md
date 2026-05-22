# Tracker Analyze Task

Analyze the Amazon Clone specification and produce a structured technical analysis.

## Your Role

You are a technical analyst studying a feature request. Your job is to break down what needs to be built, identify key decisions, and surface risks.

## Reference Spec

Read the full specification at: `benchmarks/amazon-clone/spec.md`

The spec defines 10 required user flows, quality standards, and framework constraints for an Amazon clone MVP.

## Task

Produce a 1-page analysis document with the following sections:

### 1. Feature Classification

Classify each of the 10 user flows into:

**Must Have (MVP Critical)**
- Core flows without which the app is unusable
- Example: Without checkout, there's no point to the cart

**Should Have (Core Experience)**
- Important flows that affect user experience
- Example: Wishlist is useful but cart works without it

**Could Have (Nice to Have)**
- Enhancement features
- Example: Product recommendations, advanced search filters

**Won't Have (Out of Scope)**
- Features that would be nice but we explicitly exclude
- Example: Real payment processing, multi-vendor marketplace

### 2. Architecture Decisions

Identify the key technical decisions:

**Frontend**
- SPA vs MPA
- State management approach
- Component architecture

**Backend**
- API design (REST vs GraphQL)
- Data storage (in-memory, SQLite, file-based)
- Authentication strategy

**Infrastructure**
- Server requirements
- Database requirements
- Frontend serving strategy

For each decision, state:
- The recommendation
- The trade-off being made
- Why this choice over alternatives

### 3. Data Model Summary

Diagram the relationships between:
- Products
- Users
- Carts
- Orders
- Reviews
- Wishlists

What are the ownership relationships? What cascades on delete?

### 4. Technical Approach

For the core MVP, describe:

**Simplest viable path for each flow:**

1. **Browse**: Static product list, client-side filtering
2. **Search**: Basic string match, no full-text search
3. **Cart**: Local state + server sync
4. **Auth**: Simple session-based auth with cookies
5. **Checkout**: Single-page form, validation before submit
6. **Orders**: Stored in-memory or SQLite, user-scoped queries
7. **Reviews**: Append-only, no editing/deleting
8. **Wishlist**: User-scoped product ID list
9. **Admin**: Basic CRUD forms, no bulk operations

**What you're NOT building:**
- Full-text search
- Real payment integration
- Email notifications
- Inventory alerts
- Advanced analytics
- Multi-tenant / multi-vendor

### 5. Risks and Mitigations

Identify 3-5 key risks and how you'd mitigate them:

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Scope creep | High | Medium | Stick to spec, defer features |

### 6. Phased Approach

If time permits, how would you phase the build?

**Phase 1 (MVP):** Core flows only, in-memory data
**Phase 2:** Persistence, auth
**Phase 3:** Polish, edge cases, performance

## Output

Write the analysis to: `ANALYSIS.md`

Keep it to 1 page (800-1000 words). Be direct — no fluff, no caveats.

Decision-makers should be able to read this and understand:
1. What we're building
2. How we're building it
3. What could go wrong
4. When to escalate