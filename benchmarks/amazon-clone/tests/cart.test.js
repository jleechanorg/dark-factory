const request = require('supertest');
const app = require('../src/server');
const { resetDB } = require('../src/models/db');

describe('Cart API', () => {
  beforeEach(() => {
    resetDB();
  });

  test('GET /api/cart returns empty items initially', async () => {
    const res = await request(app)
      .get('/api/cart')
      .expect(200);

    expect(res.body).toHaveProperty('items');
    expect(res.body.items.length).toBe(0);
  });

  test('POST /api/cart/items adds an item to cart', async () => {
    const agent = request.agent(app);
    // Add first item
    await agent
      .post('/api/cart/items')
      .send({ productId: 'p1', quantity: 2 })
      .expect(201);

    const res = await agent
      .get('/api/cart')
      .expect(200);

    expect(res.body.items.length).toBe(1);
    expect(res.body.items[0].productId).toBe('p1');
    expect(res.body.items[0].quantity).toBe(2);
    expect(res.body.items[0].product.id).toBe('p1');
  });

  test('PUT /api/cart/items/:productId updates quantity', async () => {
    const agent = request.agent(app);
    // Setup
    await agent
      .post('/api/cart/items')
      .send({ productId: 'p1', quantity: 1 });

    // Update to 5
    await agent
      .put('/api/cart/items/p1')
      .send({ quantity: 5 })
      .expect(200);

    const res = await agent
      .get('/api/cart')
      .expect(200);

    expect(res.body.items[0].quantity).toBe(5);
  });

  test('DELETE /api/cart/items/:productId removes item from cart', async () => {
    const agent = request.agent(app);
    // Setup
    await agent
      .post('/api/cart/items')
      .send({ productId: 'p1', quantity: 1 });

    // Delete
    await agent
      .delete('/api/cart/items/p1')
      .expect(200);

    const res = await agent
      .get('/api/cart')
      .expect(200);

    expect(res.body.items.length).toBe(0);
  });

  test('DELETE /api/cart clears the entire cart', async () => {
    const agent = request.agent(app);
    await agent
      .post('/api/cart/items')
      .send({ productId: 'p1', quantity: 1 });
    await agent
      .post('/api/cart/items')
      .send({ productId: 'p2', quantity: 1 });

    await agent
      .delete('/api/cart')
      .expect(200);

    const res = await agent
      .get('/api/cart')
      .expect(200);

    expect(res.body.items.length).toBe(0);
  });
});
