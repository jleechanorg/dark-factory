const express = require('express');
const router = express.Router();
const Cart = require('../models/cart');
const Product = require('../models/product');
const { authMiddleware } = require('./middleware');

router.get('/', authMiddleware, (req, res) => {
  const cart = Cart.get(req.user.id);
  
  // Enrich items with full product details
  const enrichedItems = cart.items.map(item => {
    const product = Product.findById(item.productId);
    return {
      ...item,
      product: product || null
    };
  }).filter(item => item.product !== null); // Filter out any deleted products

  return res.json({ items: enrichedItems });
});

router.post('/items', authMiddleware, (req, res) => {
  const { productId, quantity } = req.body;
  if (!productId) {
    return res.status(400).json({ error: 'Product ID is required' });
  }

  try {
    const qty = quantity !== undefined ? parseInt(quantity, 10) : 1;
    const cart = Cart.addItem(req.user.id, productId, qty);
    return res.status(201).json(cart);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

router.put('/items/:productId', authMiddleware, (req, res) => {
  const { productId } = req.params;
  const { quantity } = req.body;

  if (quantity === undefined) {
    return res.status(400).json({ error: 'Quantity is required' });
  }

  try {
    const cart = Cart.updateItem(req.user.id, productId, quantity);
    return res.json(cart);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

router.delete('/items/:productId', authMiddleware, (req, res) => {
  const { productId } = req.params;
  try {
    const cart = Cart.removeItem(req.user.id, productId);
    return res.json(cart);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

router.delete('/', authMiddleware, (req, res) => {
  const cart = Cart.clear(req.user.id);
  return res.json(cart);
});

module.exports = router;
