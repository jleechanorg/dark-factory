const express = require('express');
const router = express.Router();
const Order = require('../models/order');
const Cart = require('../models/cart');
const { authMiddleware } = require('./middleware');

router.post('/', authMiddleware, (req, res) => {
  const cart = Cart.get(req.user.id);
  if (!cart || cart.items.length === 0) {
    return res.status(400).json({ error: 'Cannot checkout with an empty cart' });
  }

  try {
    const orderData = {
      ...req.body,
      items: cart.items
    };

    const newOrder = Order.create(req.user.id, orderData);
    return res.status(201).json(newOrder);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

router.get('/', authMiddleware, (req, res) => {
  const orders = Order.list(req.user.id);
  return res.json(orders);
});

module.exports = router;
