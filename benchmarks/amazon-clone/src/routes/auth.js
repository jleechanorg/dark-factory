const express = require('express');
const router = express.Router();
const { registerUser, loginUser, logoutUser, sanitizeUser } = require('../services/authService');
const { setSessionCookie, clearSessionCookie, sendSuccess, sendError } = require('../utils/response');
const { requireAuth, requireRole } = require('./middleware');
const { usersRepo, wishlistsRepo, cartsRepo } = require('../storage/firestoreStore');

router.post('/register', async (req, res) => {
  try {
    const result = await registerUser(req.body || {});
    setSessionCookie(res, result.session.id);
    return sendSuccess(res, 201, {
      user: sanitizeUser(result.user),
      session: {
        id: result.session.id,
        userId: result.session.userId,
        role: result.session.role,
      },
    });
  } catch (error) {
    return sendError(res, 400, 'REGISTRATION_FAILED', error.message);
  }
});

router.post('/signup', async (req, res) => {
  return router.handle({
    ...req,
    method: 'POST',
    url: '/register',
    body: req.body,
    originalUrl: `${req.baseUrl}/signup`,
  }, res, () => {});
});

router.post('/login', async (req, res) => {
  try {
    const result = await loginUser(req.body || {});
    setSessionCookie(res, result.session.id);
    return sendSuccess(res, 200, {
      user: sanitizeUser(result.user),
      session: {
        id: result.session.id,
        userId: result.session.userId,
        role: result.session.role,
      },
    });
  } catch (error) {
    return sendError(res, 401, 'AUTH_FAILED', error.message);
  }
});

router.post('/signin', async (req, res) => {
  return router.handle({
    ...req,
    method: 'POST',
    url: '/login',
    body: req.body,
    originalUrl: `${req.baseUrl}/signin`,
  }, res, () => {});
});

router.post('/logout', async (req, res) => {
  try {
    await logoutUser(req.cookies && req.cookies.amazon_session);
    clearSessionCookie(res);
    return sendSuccess(res, 200, { ok: true });
  } catch (error) {
    return sendError(res, 500, 'LOGOUT_FAILED', error.message);
  }
});

router.post('/signout', async (req, res) => {
  return router.handle({
    ...req,
    method: 'POST',
    url: '/logout',
    originalUrl: `${req.baseUrl}/signout`,
  }, res, () => {});
});

router.get('/me', requireAuth, async (req, res) => {
  if (!req.user) {
    return sendSuccess(res, 200, { user: null });
  }
  const user = sanitizeUser(await usersRepo.findById(req.user.id));
  return sendSuccess(res, 200, { user });
});

router.get('/wishlist', requireAuth, async (req, res) => {
  const wishlist = await wishlistsRepo.asProducts(req.user.id);
  return sendSuccess(res, 200, { items: wishlist });
});

router.get('/session', async (req, res) => {
  return res.redirect(307, '/session');
});

router.get('/protected-demo', requireAuth, (req, res) => {
  return sendSuccess(res, 200, { ok: true });
});

router.get('/seller-demo', requireRole('seller', 'admin'), (req, res) => {
  return sendSuccess(res, 200, { ok: true, role: req.user.role });
});

router.get('/admin-demo', requireRole('admin'), (req, res) => {
  return sendSuccess(res, 200, { ok: true, role: req.user.role });
});

module.exports = router;
