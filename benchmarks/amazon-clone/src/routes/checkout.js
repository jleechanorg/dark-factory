const express = require('express');
const router = express.Router();
const Order = require('../models/order');
const Cart = require('../models/cart');
const Product = require('../models/product');
const { authMiddleware } = require('./middleware');

let checkoutAttempts = 0;
let ordersCreated = 0;

const parseCheckoutPayload = (payload, cart, user) => {
  if (payload && payload.shippingAddress && payload.payment) {
    const {
      shippingAddress,
      payment
    } = payload;

    return {
      email: user.email,
      fullName: shippingAddress.name,
      address: shippingAddress.line1,
      city: shippingAddress.city,
      state: shippingAddress.state || shippingAddress.region,
      zip: shippingAddress.postalCode || shippingAddress.zip,
      cardNumber: payment.cardNumber,
      expiryDate: payment.expiry || payment.expiryDate,
      cvv: payment.cvv,
      items: cart.items,
      metrics: {
        checkoutAttempts,
        ordersCreated
      }
    };
  }

  if (payload && payload.email && payload.cardNumber) {
    return {
      ...payload,
      email: payload.email,
      fullName: payload.fullName,
      address: payload.address,
      city: payload.city,
      state: payload.state,
      zip: payload.zip,
      cardNumber: payload.cardNumber,
      expiryDate: payload.expiryDate || payload.expiry,
      cvv: payload.cvv,
      items: cart.items
    };
  }

  throw new Error('Checkout payload missing required shipping/payment details');
};

router.post('/summary', authMiddleware, (req, res) => {
  const cart = Cart.get(req.user.id);
  const subtotal = cart.items.reduce((sum, item) => {
    const product = Product.findById(item.productId);
    return sum + ((product ? Math.round(product.price * 100) : 0) * item.quantity);
  }, 0);

  const discount = cart.couponCode ? Math.round(subtotal * 0.1) : 0;
  const shipping = subtotal > 0 ? (subtotal >= 2500 ? 0 : 500) : 0;
  const taxable = Math.max(0, subtotal - discount);
  const tax = Math.round(taxable * 0.08);

  return res.status(200).json({
    subtotalCents: subtotal,
    discountCents: discount,
    shippingCents: shipping,
    taxCents: tax,
    grandTotalCents: subtotal - discount + shipping + tax,
    itemsCount: cart.items.length
  });
});

router.post('/', authMiddleware, (req, res) => {
  const cart = Cart.get(req.user.id);
  if (!cart || !cart.items || cart.items.length === 0) {
    return res.status(400).json({ error: 'Cannot checkout with an empty cart' });
  }

  checkoutAttempts += 1;

  try {
    const orderData = parseCheckoutPayload(req.body, cart, req.user);
    const newOrder = Order.create(req.user.id, orderData);
    ordersCreated += 1;
    return res.status(201).json(newOrder);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

module.exports = router;
