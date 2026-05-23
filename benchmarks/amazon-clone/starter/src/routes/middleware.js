const User = require('../models/user');

function authMiddleware(req, res, next) {
  // Try to find session token in cookies or Authorization header
  let token = req.cookies && req.cookies.session_id;
  
  if (!token && req.headers.authorization) {
    const parts = req.headers.authorization.split(' ');
    if (parts.length === 2 && parts[0] === 'Bearer') {
      token = parts[1];
    } else {
      token = req.headers.authorization;
    }
  }

  if (token) {
    if (token.startsWith('guest_')) {
      req.user = { id: token, name: 'Guest User', email: 'guest@example.com', isGuest: true };
    } else {
      const user = User.findById(token);
      if (user) {
        const { passwordHash, ...safeUser } = user;
        req.user = safeUser;
      }
    }
  }

  // If no user/token found, create a temporary guest token
  if (!req.user) {
    const guestId = 'guest_' + Math.random().toString(36).substr(2, 9);
    req.user = { id: guestId, name: 'Guest User', email: 'guest@example.com', isGuest: true };
    // Set a cookie so they retain guest cart across requests
    res.cookie('session_id', guestId, { httpOnly: true, maxAge: 30 * 24 * 60 * 60 * 1000 });
  }

  next();
}

module.exports = { authMiddleware };
