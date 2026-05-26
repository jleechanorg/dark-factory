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

router.get('/:id', authMiddleware, (req, res) => {
  const order = Order.getById(req.user.id, req.params.id);
  if (!order) {
    return res.status(404).json({ error: 'Order not found' });
  }
  return res.json(order);
});

router.post('/:id/reorder', authMiddleware, (req, res) => {
  const order = Order.getById(req.user.id, req.params.id);
  if (!order) {
    return res.status(404).json({ error: 'Order not found' });
  }

  try {
    Cart.clear(req.user.id);
    for (const item of order.items) {
      Cart.addItem(req.user.id, item.productId, item.quantity);
    }
    return res.status(201).json({ orderId: req.params.id, status: 'restored' });
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

module.exports = router;
