const crypto = require('crypto');
const {
  usersRepo,
  sessionsRepo,
  sellerProfilesRepo,
} = require('../storage/firestoreStore');

function hashPassword(password) {
  return crypto
    .createHash('sha256')
    .update(String(password || ''))
    .digest('hex');
}

function sanitizeUser(user) {
  if (!user) {
    return null;
  }
  return {
    id: user.id,
    email: user.email,
    name: user.name,
    role: user.role || 'shopper',
    defaultAddressId: user.defaultAddressId || null,
    notificationPreferences: user.notificationPreferences || {},
    sellerProfileId: user.sellerProfileId || null,
    createdAt: user.createdAt,
    updatedAt: user.updatedAt,
  };
}

function userFromPayload(payload) {
  const role = payload.role || 'shopper';
  if (!['shopper', 'seller', 'admin'].includes(role)) {
    throw new Error('Invalid role');
  }

  const email = `${payload.email || ''}`.trim().toLowerCase();
  const name = `${payload.name || ''}`.trim();
  const password = `${payload.password || ''}`;

  if (!email.includes('@') || email.length < 5) {
    throw new Error('Valid email required');
  }
  if (!name || name.length < 2) {
    throw new Error('Name required');
  }
  if (password.length < 8) {
    throw new Error('Password must be at least 8 characters');
  }

  return {
    email,
    name,
    role,
    passwordHash: hashPassword(password),
  };
}

async function registerUser(payload) {
  const cleaned = userFromPayload(payload);
  const created = await usersRepo.create(cleaned);

  if (cleaned.role === 'seller') {
    const profile = await sellerProfilesRepo.create({
      userId: created.id,
      displayName: `${cleaned.name} Store`,
      supportEmail: cleaned.email,
      active: true,
    });
    await usersRepo.updateRole(created.id, 'seller');
    created.sellerProfileId = profile.id;
  }

  const safeUser = sanitizeUser(created);
  const session = await sessionsRepo.create({
    userId: safeUser.id,
    role: safeUser.role,
    email: safeUser.email,
    name: safeUser.name,
  });

  return {
    user: safeUser,
    session,
  };
}

async function loginUser(email, password) {
  const user = await usersRepo.findByEmail(`${email || ''}`);
  if (!user) {
    throw new Error('Invalid email or password');
  }
  if (user.passwordHash !== hashPassword(password || '')) {
    throw new Error('Invalid email or password');
  }

  const session = await sessionsRepo.create({
    userId: user.id,
    role: user.role || 'shopper',
    email: user.email,
    name: user.name,
  });

  return {
    user: sanitizeUser(user),
    session,
  };
}

async function logoutUser(sessionId) {
  if (!sessionId) {
    return;
  }
  await sessionsRepo.destroy(sessionId);
}

module.exports = {
  registerUser,
  loginUser,
  logoutUser,
  sanitizeUser,
  hashPassword,
};
