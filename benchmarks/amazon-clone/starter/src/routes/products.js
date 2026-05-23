const express = require('express');
const router = express.Router();
const Product = require('../models/product');
const { authMiddleware } = require('./middleware');

router.get('/', (req, res) => {
  const { category, search } = req.query;
  const products = Product.all({ category, search });
  return res.json(products);
});

router.get('/:id', (req, res) => {
  const product = Product.findById(req.params.id);
  if (!product) {
    return res.status(404).json({ error: 'Product not found' });
  }
  return res.json(product);
});

router.post('/:id/reviews', authMiddleware, (req, res) => {
  const { rating, comment, user } = req.body;
  const username = user || (req.user && !req.user.isGuest ? req.user.name : 'Anonymous');

  if (rating === undefined) {
    return res.status(400).json({ error: 'Rating is required' });
  }

  try {
    const updatedProduct = Product.addReview(req.params.id, username, rating, comment);
    return res.status(201).json(updatedProduct);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

router.put('/:id/stock', (req, res) => {
  const { stock } = req.body;
  if (stock === undefined) {
    return res.status(400).json({ error: 'Stock quantity is required' });
  }

  try {
    const updatedProduct = Product.updateStock(req.params.id, stock);
    return res.json(updatedProduct);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

module.exports = router;
