const crypto = require('crypto');
const {
  usersRepo,
  sessionsRepo,
  metricsRepo,
} = require('../storage/firestoreStore');
const {
  SESSION_COOKIE_NAME,
  SESSION_DURATION_MS,
} = require('../config/constants');
const { sendError } = require('../utils/response');

function pickSessionToken(req) {
  if (req.cookies && req.cookies[SESSION_COOKIE_NAME]) {
    return req.cookies[SESSION_COOKIE_NAME];
  }
  if (!req.headers.authorization) {
    return null;
  }

  const raw = String(req.headers.authorization).trim();
  const parts = raw.split(' ');
  if (parts.length === 2 && /^bearer$/i.test(parts[0])) {
    return parts[1];
  }
  return raw || null;
}

function createGuestId(req, sessionId) {
  const raw = `${sessionId || req.get('x-request-id') || ''}::${req.ip || '127.0.0.1'}::${req.get('user-agent') || 'agent'}`;
  return `guest_${crypto.createHash('sha256').update(raw).digest('hex').slice(0, 18)}`;
}

async function normalizeSession(req, res, next) {
  let token = pickSessionToken(req);
  let session = null;

  if (token) {
    session = await sessionsRepo.findById(token);
  }

  if (!session) {
    session = await sessionsRepo.create({
      userId: null,
      role: 'shopper',
      email: null,
      name: 'Guest Shopper',
    });
    token = session.id;
  }

  if (session.userId) {
    const persistedUser = await usersRepo.findById(session.userId);
    if (!persistedUser) {
      await sessionsRepo.destroy(session.id);
      session = await sessionsRepo.create({
        userId: null,
        role: 'shopper',
        email: null,
        name: 'Guest Shopper',
      });
      token = session.id;
    }
  }

  const isGuest = !session.userId;
  req.session = {
    id: session.id,
    userId: session.userId || null,
    role: isGuest ? 'shopper' : (session.role || 'shopper'),
    isGuest,
    email: session.email || null,
    name: session.name || 'Guest Shopper',
  };

  if (isGuest) {
    req.user = {
      id: createGuestId(req, session.id),
      email: null,
      name: session.name || 'Guest Shopper',
      role: 'shopper',
      isGuest: true,
      defaultAddressId: null,
    };
  } else {
    const persistedUser = await usersRepo.findById(session.userId);
    if (persistedUser) {
      req.user = {
        id: persistedUser.id,
        email: persistedUser.email,
        name: persistedUser.name,
        role: persistedUser.role || req.session.role || 'shopper',
        isGuest: false,
        defaultAddressId: persistedUser.defaultAddressId || null,
      };
      await sessionsRepo.touch(session.id);
    } else {
      req.user = {
        id: createGuestId(req, session.id),
        email: null,
        name: session.name || 'Guest Shopper',
        role: 'shopper',
        isGuest: true,
        defaultAddressId: null,
      };
    }
  }

  req.isAuthenticated = Boolean(req.user && !req.user.isGuest);
  req.sessionCookie = session.id;
  res.cookie(SESSION_COOKIE_NAME, req.session.id, {
    httpOnly: true,
    sameSite: 'lax',
    path: '/',
    maxAge: SESSION_DURATION_MS,
  });
  return next();
}

function requireAuth(req, res, next) {
  if (!req.user || req.user.isGuest) {
    return sendError(res, 401, 'AUTH_REQUIRED', 'Authentication required');
  }
  return next();
}

function requireRole(...roles) {
  const allowed = new Set(roles);
  return (req, res, next) => {
    if (!req.user || req.user.isGuest) {
      return sendError(res, 401, 'AUTH_REQUIRED', 'Authentication required');
    }
    if (!allowed.has(req.user.role)) {
      return sendError(res, 403, 'FORBIDDEN', 'Permission denied');
    }
    return next();
  };
}

function requestGuard(req, res, next) {
  const startedAt = Date.now();
  Promise.resolve()
    .then(() => metricsRepo.inc('requestCount', 1))
    .catch(() => undefined);

  res.on('finish', () => {
    const elapsed = Date.now() - startedAt;
    metricsRepo.recordLatency(elapsed).catch(() => undefined);
    if (res.statusCode >= 400) {
      metricsRepo.inc('errorCount', 1).catch(() => undefined);
    }
  });

  return next();
}

module.exports = {
  normalizeSession,
  requireAuth,
  requireRole,
  requestGuard,
  authMiddleware: requireAuth,
  optionalAuth: normalizeSession,
};
