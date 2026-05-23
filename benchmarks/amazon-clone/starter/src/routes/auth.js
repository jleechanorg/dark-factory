const express = require('express');
const router = express.Router();
const User = require('../models/user');
const { authMiddleware } = require('./middleware');

router.post('/signup', (req, res) => {
  const { email, password, name } = req.body;
  if (!email || !password || !name) {
    return res.status(400).json({ error: 'Email, password, and name are required' });
  }

  try {
    const newUser = User.create(email, password, name);
    res.cookie('session_id', newUser.id, { httpOnly: true, maxAge: 30 * 24 * 60 * 60 * 1000 });
    return res.status(201).json({ user: newUser, token: newUser.id });
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

router.post('/signin', (req, res) => {
  const { email, password } = req.body;
  if (!email || !password) {
    return res.status(400).json({ error: 'Email and password are required' });
  }

  const user = User.findByEmail(email);
  if (!user || !User.verifyPassword(user, password)) {
    return res.status(401).json({ error: 'Invalid email or password' });
  }

  const { passwordHash, ...safeUser } = user;
  res.cookie('session_id', safeUser.id, { httpOnly: true, maxAge: 30 * 24 * 60 * 60 * 1000 });
  return res.json({ user: safeUser, token: safeUser.id });
});

router.post('/signout', (req, res) => {
  res.clearCookie('session_id');
  return res.json({ message: 'Signed out successfully' });
});

router.get('/me', authMiddleware, (req, res) => {
  if (req.user.isGuest) {
    return res.json({ user: null });
  }
  return res.json({ user: req.user });
});

router.get('/wishlist', authMiddleware, (req, res) => {
  if (req.user.isGuest) {
    return res.status(401).json({ error: 'Authentication required for wishlist' });
  }
  const wishlist = User.getWishlist(req.user.id);
  return res.json(wishlist);
});

router.post('/wishlist', authMiddleware, (req, res) => {
  if (req.user.isGuest) {
    return res.status(401).json({ error: 'Authentication required for wishlist' });
  }
  const { productId } = req.body;
  if (!productId) {
    return res.status(400).json({ error: 'Product ID is required' });
  }
  try {
    User.addToWishlist(req.user.id, productId);
    const wishlist = User.getWishlist(req.user.id);
    return res.json(wishlist);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

router.delete('/wishlist/:productId', authMiddleware, (req, res) => {
  if (req.user.isGuest) {
    return res.status(401).json({ error: 'Authentication required for wishlist' });
  }
  const { productId } = req.params;
  try {
    User.removeFromWishlist(req.user.id, productId);
    const wishlist = User.getWishlist(req.user.id);
    return res.json(wishlist);
  } catch (error) {
    return res.status(400).json({ error: error.message });
  }
});

module.exports = router;
