# Kilroy Spec Task

Create a detailed specification document for the Amazon Clone MVP.

## Your Role

You are a senior product manager creating a spec to hand off to a developer. Be specific — vague specs lead to mediocre implementations.

## Task

Write a comprehensive SPEC.md that covers:

### 1. UI Layout

For each major view, describe:
- Header (logo, search bar, navigation links, cart icon)
- Main content area layout
- Footer content
- Responsive breakpoints

Specific views to define:
- **Home Page**: Hero banner, category grid, featured products, deals section
- **Product Listing**: Grid/list toggle, filters sidebar, sort options, pagination
- **Product Detail**: Image gallery, title, price, description, add to cart, reviews
- **Cart**: Line items, quantity controls, subtotal, proceed to checkout
- **Checkout**: Multi-step form (shipping, payment, review, confirmation)
- **User Account**: Login form, registration form, profile page, order history
- **Admin Panel**: Product list, add/edit form, inventory management

### 2. Data Model

Define the schema for all entities:

**Product**
```
- id: string (UUID)
- name: string
- description: string
- price: number (in cents)
- category: string
- imageUrl: string
- stock: number
- rating: number (1-5)
- reviewCount: number
- createdAt: timestamp
```

**Cart**
```
- id: string (UUID)
- userId: string (null for guest)
- items: array of {productId, quantity}
- updatedAt: timestamp
```

**Order**
```
- id: string (UUID)
- userId: string
- items: array of {productId, quantity, priceAtTime}
- total: number
- status: 'pending' | 'processing' | 'shipped' | 'delivered'
- shippingAddress: object
- createdAt: timestamp
```

**User**
```
- id: string (UUID)
- email: string (unique)
- passwordHash: string
- name: string
- createdAt: timestamp
```

**Review**
```
- id: string (UUID)
- productId: string
- userId: string
- rating: number (1-5)
- comment: string
- createdAt: timestamp
```

**Wishlist**
```
- id: string (UUID)
- userId: string
- productIds: array of strings
```

### 3. User Flows

For each of the 10 required flows, describe:

**Browse Products**
- Entry point: home page
- Actions: click category, view product grid, scroll pagination
- Success: product cards display correctly

**Search Products**
- Entry point: search bar in header
- Actions: type query, see autocomplete, click result
- Success: results page shows matching products

**View Product Details**
- Entry point: click product from listing
- Actions: view images, read description, read reviews, add to cart
- Success: all information displays, add to cart works

**Shopping Cart**
- Entry point: cart icon or add-to-cart
- Actions: view items, change quantity, remove item, checkout
- Success: totals update, checkout button works

**User Authentication**
- Entry point: header sign in link
- Actions: enter credentials, submit, see confirmation
- Success: session persists, user-specific features work

**Checkout Process**
- Entry point: cart checkout button
- Actions: enter shipping, enter payment, review, confirm
- Success: order created, confirmation shown, email sent

**Order History**
- Entry point: user account dropdown
- Actions: view list, click order, see details
- Success: past orders display with correct data

**Product Reviews**
- Entry point: product detail page reviews section
- Actions: see existing reviews, submit new review with rating
- Success: review appears, rating aggregated

**Wishlist**
- Entry point: product detail or listing
- Actions: add to wishlist, view wishlist, remove from wishlist
- Success: wishlist persists across sessions

**Admin Inventory**
- Entry point: admin panel link (only for admin users)
- Actions: view products, add product, edit product, delete product
- Success: changes persist and reflect in storefront

### 4. API Endpoints

Define all REST endpoints:

**Products**
- `GET /api/products` - List all products (with pagination, search, filters)
- `GET /api/products/:id` - Get single product
- `POST /api/products` - Create product (admin)
- `PUT /api/products/:id` - Update product (admin)
- `DELETE /api/products/:id` - Delete product (admin)

**Cart**
- `GET /api/cart` - Get current cart
- `POST /api/cart/items` - Add item to cart
- `PUT /api/cart/items/:productId` - Update quantity
- `DELETE /api/cart/items/:productId` - Remove item
- `DELETE /api/cart` - Clear cart

**Orders**
- `GET /api/orders` - List user's orders
- `GET /api/orders/:id` - Get order details
- `POST /api/orders` - Create order from cart

**Auth**
- `POST /api/auth/register` - Create account
- `POST /api/auth/login` - Sign in
- `POST /api/auth/logout` - Sign out
- `GET /api/auth/me` - Get current user

**Reviews**
- `GET /api/products/:id/reviews` - Get product reviews
- `POST /api/products/:id/reviews` - Submit review

**Wishlist**
- `GET /api/wishlist` - Get user's wishlist
- `POST /api/wishlist` - Add product
- `DELETE /api/wishlist/:productId` - Remove product

### 5. Acceptance Criteria

For each feature, define specific, testable criteria:

**Browse**
- [ ] Products display in grid layout
- [ ] Category filter works
- [ ] Pagination loads more products

**Search**
- [ ] Search returns products matching query
- [ ] Empty search shows all products
- [ ] No results shows helpful message

**Cart**
- [ ] Adding item increases cart count
- [ ] Quantity update recalculates total
- [ ] Removing last item shows empty cart state

**Auth**
- [ ] Registration creates account
- [ ] Login returns session
- [ ] Logout clears session

... (and so on for all 10 flows)

## Output

Write the spec to: `SPEC.md`

Use markdown with clear headings, code blocks for data schemas, and checkbox lists for acceptance criteria.

Be specific. "Works well" is not an acceptance criterion. "Button click triggers API call and updates cart count within 500ms" is.