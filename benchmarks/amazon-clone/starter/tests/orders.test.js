const request = require('supertest');
const app = require('../src/server');
const { resetDB } = require('../src/models/db');

describe('Orders API', () => {
  beforeEach(() => {
    resetDB();
  });

  const validCheckout = {
    email: 'test@example.com',
    fullName: 'Test Buyer',
    address: '1234 Main Street, Seattle',
    city: 'Seattle',
    state: 'WA',
    zip: '98101',
    cardNumber: '1111222233334444',
    expiryDate: '12/28',
    cvv: '123'
  };

  test('POST /api/orders rejects checkout with empty cart', async () => {
    await request(app)
      .post('/api/orders')
      .send(validCheckout)
      .expect(400);
  });

  test('POST /api/orders completes checkout, masks card, and reduces product stock', async () => {
    // Add product to cart first
    const agent = request.agent(app);
    await agent
      .post('/api/cart/items')
      .send({ productId: 'p1', quantity: 2 })
      .expect(201);

    // Complete Checkout
    const res = await agent
      .post('/api/orders')
      .send(validCheckout)
      .expect(201);

    expect(res.body).toHaveProperty('id');
    expect(res.body.id.length).toBeGreaterThanOrEqual(8);
    expect(res.body.maskedCard).toBe('****4444');
    expect(res.body.total).toBe(99.98); // 49.99 * 2
    expect(res.body.items.length).toBe(1);
    expect(res.body.items[0].productId).toBe('p1');
    expect(res.body.items[0].quantity).toBe(2);

    // Check inventory stock is reduced: Echo Dot had 25, should be 23 now
    const prodRes = await agent
      .get('/api/products/p1')
      .expect(200);
    expect(prodRes.body.stock).toBe(23);

    // Check cart is cleared
    const cartRes = await agent
      .get('/api/cart')
      .expect(200);
    expect(cartRes.body.items.length).toBe(0);

    // Check order is in history list
    const historyRes = await agent
      .get('/api/orders')
      .expect(200);
    expect(historyRes.body.length).toBe(1);
    expect(historyRes.body[0].id).toBe(res.body.id);
  });

  test('POST /api/orders performs robust form validations', async () => {
    const agent = request.agent(app);
    await agent
      .post('/api/cart/items')
      .send({ productId: 'p1', quantity: 1 })
      .expect(201);

    // Invalid Email
    await agent
      .post('/api/orders')
      .send({ ...validCheckout, email: 'no_at_symbol' })
      .expect(400);

    // Name too short
    await agent
      .post('/api/orders')
      .send({ ...validCheckout, fullName: 'A' })
      .expect(400);

    // Address too short
    await agent
      .post('/api/orders')
      .send({ ...validCheckout, address: 'Short' })
      .expect(400);

    // ZIP not numeric
    await agent
      .post('/api/orders')
      .send({ ...validCheckout, zip: 'abcde' })
      .expect(400);

    // Card number not 16 digits
    await agent
      .post('/api/orders')
      .send({ ...validCheckout, cardNumber: '123456789' })
      .expect(400);

    // Expiry date wrong format
    await agent
      .post('/api/orders')
      .send({ ...validCheckout, expiryDate: '12-28' })
      .expect(400);

    // CVV invalid
    await agent
      .post('/api/orders')
      .send({ ...validCheckout, cvv: '12' })
      .expect(400);
  });
});
