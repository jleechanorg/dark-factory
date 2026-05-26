const request = require('supertest');
const app = require('../src/server');
const { resetDB } = require('../src/models/db');

describe('Products API', () => {
  beforeEach(() => {
    resetDB();
  });

  test('GET /api/products returns all products', async () => {
    const res = await request(app)
      .get('/api/products')
      .expect(200);

    expect(Array.isArray(res.body)).toBe(true);
    expect(res.body.length).toBe(5);
    expect(res.body[0]).toHaveProperty('id');
    expect(res.body[0]).toHaveProperty('title');
    expect(res.body[0]).toHaveProperty('price');
  });

  test('GET /api/products?category=Electronics filters by category', async () => {
    const res = await request(app)
      .get('/api/products?category=Electronics')
      .expect(200);

    expect(res.body.length).toBe(2);
    expect(res.body[0].category).toBe('Electronics');
  });

  test('GET /api/products?search=Sony matches title/description', async () => {
    const res = await request(app)
      .get('/api/products?search=Sony')
      .expect(200);

    expect(res.body.length).toBe(1);
    expect(res.body[0].id).toBe('p2');
  });

  test('GET /api/products/:id retrieves a product detail', async () => {
    const res = await request(app)
      .get('/api/products/p3')
      .expect(200);

    expect(res.body.id).toBe('p3');
    expect(res.body.title).toContain('Atomic Habits');
  });

  test('GET /api/products/:id returns 404 for unknown product', async () => {
    await request(app)
      .get('/api/products/unknown')
      .expect(404);
  });

  test('POST /api/products/:id/reviews adds review and recalculates average rating', async () => {
    const reviewData = {
      rating: 5,
      comment: 'Superb product!',
      user: 'Test Reviewer'
    };

    const res = await request(app)
      .post('/api/products/p1/reviews')
      .send(reviewData)
      .expect(201);

    expect(res.body.reviews.length).toBe(3);
    const added = res.body.reviews.find(r => r.user === 'Test Reviewer');
    expect(added).toBeDefined();
    expect(added.rating).toBe(5);
    expect(added.comment).toBe('Superb product!');
    
    // Original p1 ratings: 5, 4. Added: 5. New Average: (5+4+5)/3 = 4.7 (remains 4.7 due to rounding)
    expect(res.body.rating).toBe(4.7);
  });

  test('POST /api/products/:id/reviews validates rating between 1 and 5', async () => {
    await request(app)
      .post('/api/products/p1/reviews')
      .send({ rating: 6, comment: 'Too high' })
      .expect(400);

    await request(app)
      .post('/api/products/p1/reviews')
      .send({ rating: 0, comment: 'Too low' })
      .expect(400);
  });
});
